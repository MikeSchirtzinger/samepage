//! The room: a document of panes, and the two vocabularies for changing it.
//!
//! Authorship is enforced by audience, not by an honour-system argument. Every
//! mutation exists twice over one implementation — `put_pane` in the agent's
//! catalog, `room_put_pane` owned by the browser module — so a log entry that
//! says `you` was reached through a name the model was never shown. `read_room`
//! has no human twin and `annotate_pane` has no agent twin, which is what makes
//! a mark trustworthy: the agent cannot mark its own work as agreed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ag_ui_surface::{
    ActionAudience, Caller, ClientModule, Effect, Extension, HttpMethod, RouteDef, RouteRequest,
    RouteResponse, StateBacking, StateSnapshot, SurfaceState, SurfaceStore, ToolDef, Transport,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

use crate::catalog::Workspace;
use crate::layout::{self, Placement, Sited, Spot};
use crate::view::{self, Node};

pub const EVENT_NAME: &str = "surface.room";
pub const FOCUS_EVENT: &str = "room.pane.focused";

const MAX_PANES: usize = 24;
const MAX_TITLE: usize = 120;
const MAX_INTENT: usize = 600;
const MAX_NOTE: usize = 600;
/// A byline is a label in a pane header, not a field. Past this it is dropped.
const MAX_BYLINE: usize = 40;
/// How long `await_room` parks by default, and the longest it will.
const DEFAULT_WAIT_SECONDS: u64 = 60;
const MAX_WAIT_SECONDS: u64 = 600;
const MAX_LOG: usize = 200;
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;

/// Every node kind, in one line, repeated everywhere the agent might look.
pub const VOCABULARY: &str = "stack, row, deck, heading, text, code, list, kv, table, badge, \
     divider, button, field, link, image, source, options, embed, html, diagram";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    You,
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
    fn word(self) -> &'static str {
        match self {
            Author::You => "you",
            Author::Agent => "agent",
            Author::Companion => "companion",
        }
    }

    /// The byline for a write that arrived through an agent-audience action.
    ///
    /// [`Caller::Human`] cannot reach one of those — the dispatcher refuses it
    /// — so it is unreachable here; it maps to the agent rather than silently
    /// crediting the person with a machine's writing. [`Caller::Unknown`] is
    /// refused before dispatch for the same reason and lands in the same arm.
    fn of(caller: Caller) -> Author {
        match caller {
            Caller::Companion => Author::Companion,
            Caller::Agent | Caller::Human | Caller::Unknown => Author::Agent,
        }
    }
}

/// The byline as a reader sees it: the category the host stands behind, then
/// the self-declared name when there is one.
///
/// Category first on purpose. Leading with the name would invite reading a
/// label the caller chose for itself as an established identity, which is the
/// one thing this room must not blur.
fn credit(author: Author, name: Option<&str>) -> String {
    match name {
        Some(name) => format!("{} “{name}”", author.word()),
        None => author.word().to_string(),
    }
}

/// Who a write is credited to: the trust category, plus the display name the
/// caller announced on the way in, when it announced one.
///
/// The two are deliberately not the same field. [`Author`] is the claim the
/// host stands behind — it comes from the transport the call arrived on and
/// nothing a caller sends can change it. The name is self-declared: an agent
/// attaching over `/mcp` picks its own `clientInfo.name`. Keeping the name
/// beside the category rather than in place of it means a nicer byline can
/// never quietly upgrade a claim — "Claude Code" still renders as an agent,
/// and a companion that calls itself anything at all still renders as a
/// companion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Byline {
    pub author: Author,
    pub name: Option<String>,
}

impl Byline {
    /// The person, who never needs a name: the room only has one of them and
    /// the surface already addresses them as "you".
    fn you() -> Self {
        Self {
            author: Author::You,
            name: None,
        }
    }

    /// A display name is decoration, so it fails soft: too long, blank, or
    /// carrying control characters and it is simply dropped and the category
    /// word stands in. A write is never refused over its byline.
    fn named(author: Author, name: Option<String>) -> Self {
        let name = name.map(|name| name.trim().to_string()).filter(|name| {
            !name.is_empty()
                && name.chars().count() <= MAX_BYLINE
                && !name.chars().any(char::is_control)
        });
        Self { author, name }
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
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "none" => Ok(Mark::None),
            "question" => Ok(Mark::Question),
            "important" => Ok(Mark::Important),
            "agree" => Ok(Mark::Agree),
            "disagree" => Ok(Mark::Disagree),
            other => Err(format!("unknown mark {other:?}")),
        }
    }

    fn describe(self) -> Option<&'static str> {
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
/// 1 → 2 replaced `span`/`height` tokens with a free rectangle per pane. A
/// version-1 document is migrated in [`migrate`] on the way in and rewritten,
/// so an old room opens where it left off rather than refusing.
const SCHEMA_VERSION: u32 = 2;

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
    fn summarize(&self) -> String {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_name: Option<String>,
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
    #[serde(default)]
    pub note: String,
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct LogEntry {
    pub revision: u64,
    pub by: Author,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_name: Option<String>,
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

impl RoomDoc {
    fn seed() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            intent: "Get on the same page about ag-ui-rust. Nothing is decided about \
                     what this room becomes."
                .to_string(),
            theme: Theme::default(),
            panes: vec![
                Pane {
                    id: "start".to_string(),
                    title: "Where do you want to take this?".to_string(),
                    author: Author::Agent,
                    by_name: None,
                    spot: Spot::new(0.0, 0.0, layout::DEFAULT_W, 340.0),
                    pinned: false,
                    mark: Mark::None,
                    note: String::new(),
                    revision: 0,
                    view: Node::Stack {
                        gap: None,
                        children: vec![
                            Node::Text {
                                text: "This room is empty on purpose. Every pane here — \
                                       including this one — is data the agent wrote and can \
                                       rewrite. Pick a direction, or type your own; the room \
                                       reshapes around it."
                                    .to_string(),
                                tone: view::Tone::Muted,
                            },
                            Node::Row {
                                gap: None,
                                wrap: Some(true),
                                children: vec![
                                    Node::Button {
                                        label: "Review the code together".to_string(),
                                        ask: "Put the parts of this repo I'd need to \
                                              understand first into the room as source panes, \
                                              and tell me what each one is for."
                                            .to_string(),
                                        tone: view::Tone::Accent,
                                    },
                                    Node::Button {
                                        label: "Show me what runs".to_string(),
                                        ask: "Walk me through the options pane — what is each \
                                              of these examples, and which two are worth \
                                              opening right now?"
                                            .to_string(),
                                        tone: view::Tone::Neutral,
                                    },
                                    Node::Button {
                                        label: "Change how this looks".to_string(),
                                        ask: "Show me what you can change about this room's \
                                              appearance, then try a version you think reads \
                                              better."
                                            .to_string(),
                                        tone: view::Tone::Neutral,
                                    },
                                    Node::Button {
                                        label: "Build me a new pane".to_string(),
                                        ask: "Invent a pane this room doesn't have yet that \
                                              would help us think about this project, and put \
                                              it up."
                                            .to_string(),
                                        tone: view::Tone::Neutral,
                                    },
                                ],
                            },
                            Node::Divider,
                            Node::Text {
                                text: "Drag a pane by its title to move it. The ? ! ✓ ✗ \
                                       buttons mark a pane for the agent — that channel is \
                                       yours alone, the agent cannot write it."
                                    .to_string(),
                                tone: view::Tone::Muted,
                            },
                        ],
                    },
                },
                Pane {
                    id: "options".to_string(),
                    title: "What this workspace can run".to_string(),
                    author: Author::Agent,
                    by_name: None,
                    spot: Spot::new(layout::DEFAULT_W + 20.0, 0.0, layout::DEFAULT_W, 460.0),
                    pinned: false,
                    mark: Mark::None,
                    note: String::new(),
                    revision: 0,
                    view: Node::Stack {
                        gap: None,
                        children: vec![
                            Node::Text {
                                text: "Discovered from the workspace manifest, not from a \
                                       list someone typed. A green dot means something \
                                       answered on that port just now."
                                    .to_string(),
                                tone: view::Tone::Muted,
                            },
                            Node::Options { filter: None },
                        ],
                    },
                },
            ],
            log: Vec::new(),
        }
    }
}

pub struct RoomState {
    transport: Transport,
    path: PathBuf,
    workspace: Arc<Workspace>,
    doc: Mutex<RoomDoc>,
    /// Where the agent's last `read_room` stopped. Session state, deliberately
    /// not persisted: a delta is "since *you* last looked", and after a restart
    /// the agent has not looked at all.
    read_cursor: Mutex<u64>,
    /// Who the runtime last said was calling, via [`Surface::note_caller`].
    /// Read while applying an agent-audience action to pick its byline, so a
    /// companion's panes are not signed by this room's agent.
    caller: Mutex<Caller>,
    /// The name that caller announced when it attached, if any. Read beside
    /// [`RoomState::caller`] to build the byline.
    caller_name: Mutex<Option<String>>,
    /// Woken after every committed change so `await_room` can park instead of
    /// poll. The room's whole premise is that the person writes on the page and
    /// the agent answers; without this the agent only finds out by asking again,
    /// which at conversation speed means it never finds out at all.
    changed: tokio::sync::Notify,
}

impl RoomState {
    pub fn open(
        transport: Transport,
        path: PathBuf,
        workspace: Arc<Workspace>,
    ) -> Result<Arc<Self>, String> {
        let doc = if path.exists() {
            load(&path)?
        } else {
            let seeded = RoomDoc::seed();
            persist(&path, &seeded)?;
            seeded
        };
        for pane in &doc.panes {
            view::validate(&pane.view)
                .map_err(|error| format!("stored pane {} is invalid: {error}", pane.id))?;
        }
        Ok(Arc::new(Self {
            transport,
            path,
            workspace,
            doc: Mutex::new(doc),
            read_cursor: Mutex::new(0),
            caller: Mutex::new(Caller::Agent),
            caller_name: Mutex::new(None),
            changed: tokio::sync::Notify::new(),
        }))
    }

    /// The byline for a write arriving through an agent-audience action.
    fn agent_byline(&self) -> Byline {
        Byline::named(
            Author::of(*self.caller.lock()),
            self.caller_name.lock().clone(),
        )
    }

    /// The wire shape: the stored document with every host-resolved node
    /// replaced by what it currently stands for.
    fn resolved(&self) -> JsonValue {
        let doc = self.doc.lock();
        let panes: Vec<JsonValue> = doc
            .panes
            .iter()
            .map(|pane| {
                json!({
                    "id": pane.id,
                    "title": pane.title,
                    "author": pane.author,
                    "by_name": pane.by_name,
                    "spot": pane.spot,
                    "pinned": pane.pinned,
                    "mark": pane.mark,
                    "note": pane.note,
                    "revision": pane.revision,
                    "view": view::resolve(&pane.view, &self.workspace),
                })
            })
            .collect();
        json!({
            "revision": doc.revision,
            "intent": doc.intent,
            "theme": doc.theme,
            "panes": panes,
            "vocabulary": VOCABULARY,
        })
    }

    fn mutate<F>(&self, expected: u64, by: Byline, apply: F) -> Result<Option<String>, String>
    where
        F: FnOnce(&mut RoomDoc, u64) -> Result<(String, String), String>,
    {
        let reply = {
            let mut live = self.doc.lock();
            if live.revision != expected {
                return Err(format!(
                    "revision conflict: you based this on {expected}, the room is at {}. \
                     Call read_room and retry.",
                    live.revision
                ));
            }
            let next = live
                .revision
                .checked_add(1)
                .ok_or_else(|| "room revision exhausted".to_string())?;
            let mut candidate = live.clone();
            let (reply, said) = apply(&mut candidate, next)?;
            candidate.revision = next;
            candidate.log.push(LogEntry {
                revision: next,
                by: by.author,
                by_name: by.name.clone(),
                said,
            });
            if candidate.log.len() > MAX_LOG {
                let overflow = candidate.log.len() - MAX_LOG;
                candidate.log.drain(0..overflow);
            }
            validate_doc(&candidate)?;
            persist(&self.path, &candidate)?;
            *live = candidate;
            format!("{reply} The room is now at revision {next}.")
        };
        self.transport.emit(EVENT_NAME, self.resolved());
        self.changed.notify_waiters();
        Ok(Some(reply))
    }

    /// The revisions an agent has not read yet that someone *else* wrote.
    ///
    /// Excluding the caller's own writes is what makes the wait usable: the
    /// natural loop is read → write → wait, and a wait that returned instantly
    /// because the agent had just written something would be a busy-loop
    /// wearing a blocking tool's clothes.
    fn unread_from_others(&self, me: &Byline) -> bool {
        let cursor = *self.read_cursor.lock();
        self.doc.lock().log.iter().any(|entry| {
            entry.revision > cursor
                && !(entry.by == me.author && entry.by_name == me.name)
        })
    }

    fn put_pane(&self, by: Byline, args: &JsonValue) -> Result<Option<String>, String> {
        let expected = required_u64(args, "expected_revision")?;
        let id = pane_id(&required_string(args, "id")?)?;
        let title = checked("title", &required_string(args, "title")?, 1, MAX_TITLE)?;
        let node: Node = serde_json::from_value(
            args.get("view")
                .cloned()
                .ok_or_else(|| "view is required".to_string())?,
        )
        .map_err(|error| {
            format!(
                "view is not a valid node tree: {error}. Node kinds are: {VOCABULARY}. \
                 GET /room/vocabulary has the full reference."
            )
        })?;
        view::validate(&node)?;
        // `size` is the one dimension an agent may express, and only in names:
        // it is asking for a shape ("this is a wide one"), not a rectangle. The
        // person's drag is what sets real geometry, through the human action.
        let size = optional_string(args, "size")?
            .map(|value| parse_size(&value))
            .transpose()?;
        let place = optional_string(args, "place")?
            .map(|value| Placement::parse(&value))
            .transpose()?;
        let summary = view::summarize(&node);

        self.mutate(expected, by.clone(), move |doc, revision| {
            let existing = doc.panes.iter().position(|pane| pane.id == id);
            let replaced = existing.is_some();
            if existing.is_none() && doc.panes.len() >= MAX_PANES {
                return Err(format!(
                    "the room already holds {MAX_PANES} panes; remove one first"
                ));
            }

            // Work out the rectangle before touching the document: a placement
            // is resolved against where everything else currently sits.
            let current = existing.map(|index| doc.panes[index].spot);
            let (width, height) = match (size, current) {
                (Some(size), _) => size,
                (None, Some(spot)) => (spot.w, spot.h),
                (None, None) => (layout::DEFAULT_W, layout::DEFAULT_H),
            };
            let spot = match (&place, current) {
                // A rewrite leaves a pane exactly where the person put it.
                // Moving it because its contents changed would quietly undo
                // their arrangement, which is the one thing free placement
                // makes it possible to lose.
                (None, Some(spot)) => Spot::new(spot.x, spot.y, width, height),
                _ => {
                    let others: Vec<Sited<'_>> = doc
                        .panes
                        .iter()
                        .filter(|pane| pane.id != id)
                        .map(|pane| Sited {
                            id: &pane.id,
                            title: &pane.title,
                            spot: pane.spot,
                        })
                        .collect();
                    layout::resolve(place.as_ref(), &others, (width, height))?
                }
            };

            let pane = match existing {
                Some(index) => {
                    let pane = &mut doc.panes[index];
                    // A human mark and note survive a content replacement. The
                    // question was asked of this pane, and the agent answering
                    // it does not get to decide the question is closed.
                    pane.title = title.clone();
                    pane.view = node.clone();
                    pane.author = by.author;
                    pane.by_name.clone_from(&by.name);
                    pane.revision = revision;
                    pane.spot = spot;
                    doc.panes[index].clone()
                }
                None => {
                    let pane = Pane {
                        id: id.clone(),
                        title: title.clone(),
                        author: by.author,
                        by_name: by.name.clone(),
                        view: node.clone(),
                        spot,
                        pinned: false,
                        mark: Mark::None,
                        note: String::new(),
                        revision,
                    };
                    doc.panes.push(pane.clone());
                    pane
                }
            };

            let verb = if replaced { "rewrote" } else { "put up" };
            Ok((
                format!(
                    "{} pane {id} — {}.",
                    if replaced { "Rewrote" } else { "Added" },
                    summary
                ),
                format!("{verb} the pane “{}” [{id}] — {summary}", pane.title),
            ))
        })
    }

    fn remove_pane(&self, by: Byline, args: &JsonValue) -> Result<Option<String>, String> {
        let expected = required_u64(args, "expected_revision")?;
        let id = pane_id(&required_string(args, "id")?)?;
        self.mutate(expected, by, move |doc, _revision| {
            let index = doc
                .panes
                .iter()
                .position(|pane| pane.id == id)
                .ok_or_else(|| format!("there is no pane {id} in the room"))?;
            if doc.panes[index].pinned {
                return Err(format!("pane {id} is pinned; unpin it before removing it"));
            }
            let removed = doc.panes.remove(index);
            Ok((
                format!("Removed pane {id}."),
                format!("took down the pane “{}” [{id}]", removed.title),
            ))
        })
    }

    /// Move, resize, or pin panes.
    ///
    /// `from_person` is the whole trust story of this function. A rectangle is
    /// only accepted from the human-audience twin, because a rectangle is what
    /// a drag produces and a drag is the person. The agent's twin takes
    /// relations and named sizes and nothing else, so no path exists by which a
    /// model can start thinking in coordinates about a surface whose point is
    /// that it does not have to.
    fn arrange(
        &self,
        by: Byline,
        from_person: bool,
        args: &JsonValue,
    ) -> Result<Option<String>, String> {
        let expected = required_u64(args, "expected_revision")?;
        let adjustments = match args.get("panes") {
            None | Some(JsonValue::Null) => Vec::new(),
            Some(JsonValue::Array(values)) => values
                .iter()
                .map(|value| {
                    let spot = match value.get("spot") {
                        None | Some(JsonValue::Null) => None,
                        Some(raw) => {
                            if !from_person {
                                return Err(SPOT_IS_NOT_YOURS.to_string());
                            }
                            Some(parse_spot(raw)?)
                        }
                    };
                    Ok(Adjustment {
                        id: pane_id(&required_string(value, "id")?)?,
                        place: optional_string(value, "place")?
                            .map(|value| Placement::parse(&value))
                            .transpose()?,
                        size: optional_string(value, "size")?
                            .map(|value| parse_size(&value))
                            .transpose()?,
                        spot,
                        pinned: optional_bool(value, "pinned")?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?,
            Some(_) => return Err("panes must be an array of adjustments".to_string()),
        };
        if adjustments.is_empty() {
            return Err(
                "arrange needs at least one pane to adjust — each one takes a place, a size, \
                 or pinned."
                    .to_string(),
            );
        }

        self.mutate(expected, by, move |doc, _revision| {
            let mut said = Vec::new();
            for adjustment in &adjustments {
                let index = doc
                    .panes
                    .iter()
                    .position(|pane| pane.id == adjustment.id)
                    .ok_or_else(|| {
                        format!("there is no pane {} in the room", adjustment.id)
                    })?;

                if let Some(pinned) = adjustment.pinned {
                    doc.panes[index].pinned = pinned;
                    said.push(format!(
                        "{} “{}”",
                        if pinned { "pinned" } else { "unpinned" },
                        doc.panes[index].title
                    ));
                }

                let moved = adjustment.spot.is_some() || adjustment.place.is_some();
                if !moved && adjustment.size.is_none() {
                    continue;
                }

                let current = doc.panes[index].spot;
                let (width, height) = adjustment.size.unwrap_or((current.w, current.h));
                let next = match (&adjustment.spot, &adjustment.place) {
                    // The person dragged it. Their rectangle is the answer.
                    (Some(spot), _) => *spot,
                    (None, Some(place)) => {
                        let others: Vec<Sited<'_>> = doc
                            .panes
                            .iter()
                            .filter(|pane| pane.id != adjustment.id)
                            .map(|pane| Sited {
                                id: &pane.id,
                                title: &pane.title,
                                spot: pane.spot,
                            })
                            .collect();
                        layout::resolve(Some(place), &others, (width, height))?
                    }
                    // Resize in place.
                    (None, None) => Spot::new(current.x, current.y, width, height),
                };
                doc.panes[index].spot = next;

                let sited: Vec<Sited<'_>> = doc
                    .panes
                    .iter()
                    .map(|pane| Sited {
                        id: &pane.id,
                        title: &pane.title,
                        spot: pane.spot,
                    })
                    .collect();
                let title = doc.panes[index].title.clone();
                if moved {
                    said.push(format!(
                        "moved “{title}” — it now sits {}",
                        layout::locate(index, &sited)
                    ));
                } else {
                    said.push(format!(
                        "resized “{title}” — it now sits {}",
                        layout::locate(index, &sited)
                    ));
                }
            }
            let said = if said.is_empty() {
                "rearranged the room (no visible change)".to_string()
            } else {
                said.join(", ")
            };
            Ok((format!("Rearranged the room: {said}."), said))
        })
    }

    fn configure(&self, by: Byline, args: &JsonValue) -> Result<Option<String>, String> {
        let expected = required_u64(args, "expected_revision")?;
        let intent = optional_string(args, "intent")?
            .map(|intent| checked("intent", &intent, 1, MAX_INTENT))
            .transpose()?;
        let theme_args = match args.get("theme") {
            None | Some(JsonValue::Null) => None,
            Some(value @ JsonValue::Object(_)) => Some(value.clone()),
            Some(_) => return Err("theme must be an object".to_string()),
        };
        if intent.is_none() && theme_args.is_none() {
            return Err("configure_room needs an intent or a theme".to_string());
        }

        self.mutate(expected, by, move |doc, _revision| {
            let mut said = Vec::new();
            if let Some(intent) = intent {
                said.push(format!("set the room's intent to “{intent}”"));
                doc.intent = intent;
            }
            if let Some(theme_args) = theme_args {
                let theme = merge_theme(&doc.theme, &theme_args)?;
                if theme != doc.theme {
                    said.push(format!("restyled the room: {}", theme.summarize()));
                    doc.theme = theme;
                }
            }
            let said = if said.is_empty() {
                "changed nothing about the room".to_string()
            } else {
                said.join(", ")
            };
            Ok((format!("Done: {said}."), said))
        })
    }

    /// Human-only. The agent has no way to write this field, which is what
    /// makes a `✓` on a pane mean something.
    fn annotate(&self, args: &JsonValue) -> Result<Option<String>, String> {
        let expected = required_u64(args, "expected_revision")?;
        let id = pane_id(&required_string(args, "id")?)?;
        let mark = optional_string(args, "mark")?
            .map(|value| Mark::parse(&value))
            .transpose()?;
        let note = optional_string(args, "note")?
            .map(|note| checked("note", &note, 0, MAX_NOTE))
            .transpose()?;
        if mark.is_none() && note.is_none() {
            return Err("annotate_pane needs a mark, a note, or both".to_string());
        }

        self.mutate(expected, Byline::you(), move |doc, _revision| {
            let pane = doc
                .panes
                .iter_mut()
                .find(|pane| pane.id == id)
                .ok_or_else(|| format!("there is no pane {id} in the room"))?;
            let mut said = Vec::new();
            if let Some(mark) = mark {
                pane.mark = mark;
                said.push(match mark.describe() {
                    Some(description) => {
                        format!("marked “{}” with {description}", pane.title)
                    }
                    None => format!("cleared the mark on “{}”", pane.title),
                });
            }
            if let Some(note) = note {
                if note.is_empty() {
                    said.push(format!("cleared their note on “{}”", pane.title));
                } else {
                    said.push(format!("noted on “{}”: “{note}”", pane.title));
                }
                pane.note = note;
            }
            let said = said.join(", ");
            Ok((format!("Recorded: {said}."), said))
        })
    }

    /// The agent's whole view of the room. Deliberately one call: state, then
    /// the delta since the agent last looked, then how to change it.
    pub fn read(&self) -> Result<String, String> {
        let doc = self.doc.lock().clone();
        let cursor = {
            let mut cursor = self.read_cursor.lock();
            let previous = *cursor;
            *cursor = doc.revision;
            previous
        };

        let mut out = format!(
            "THE ROOM — revision {}, {} pane{} on a free canvas\nIntent: {}\nAppearance: {}\n",
            doc.revision,
            doc.panes.len(),
            if doc.panes.len() == 1 { "" } else { "s" },
            doc.intent,
            doc.theme.summarize(),
        );

        if doc.panes.is_empty() {
            out.push_str("\nThe room is empty. Nothing is on the shared page yet.\n");
        } else {
            out.push_str("\nPANES, in reading order — down the canvas, left to right\n");
            let sited: Vec<Sited<'_>> = doc
                .panes
                .iter()
                .map(|pane| Sited {
                    id: &pane.id,
                    title: &pane.title,
                    spot: pane.spot,
                })
                .collect();
            for (rank, index) in layout::in_reading_order(&sited).into_iter().enumerate() {
                let pane = &doc.panes[index];
                out.push_str(&format!(
                    "{}. [{}] “{}” · by {} · {}\n",
                    rank + 1,
                    pane.id,
                    pane.title,
                    credit(pane.author, pane.by_name.as_deref()),
                    view::summarize(&pane.view),
                ));
                if let Some(mark) = pane.mark.describe() {
                    out.push_str(&format!("     MARKED {mark}\n"));
                }
                if !pane.note.is_empty() {
                    out.push_str(&format!("     their note: “{}”\n", pane.note));
                }
                if pane.pinned {
                    out.push_str("     pinned; it cannot be removed until they unpin it\n");
                }
            }
            out.push_str(&layout::describe(&sited));
        }

        if doc.panes.iter().any(|pane| view::shows_options(&pane.view)) {
            out.push_str(
                "\nWHAT IS RUNNABLE — this is the catalog they can see in the options pane, \
                 read from the workspace manifest just now\n",
            );
            out.push_str(&self.workspace.describe_options());
            out.push('\n');
        }

        let changes: Vec<&LogEntry> = doc
            .log
            .iter()
            .filter(|entry| entry.revision > cursor)
            .collect();
        out.push_str("\nCHANGED SINCE YOUR LAST READ\n");
        if cursor == 0 && doc.revision > 0 {
            out.push_str(
                "This is your first read this session, so everything above is new to you.\n",
            );
        }
        if changes.is_empty() {
            out.push_str("Nothing. The room is exactly as you left it.\n");
        } else {
            for entry in changes {
                out.push_str(&format!(
                    "- r{}: {} {}\n",
                    entry.revision,
                    credit(entry.by, entry.by_name.as_deref()),
                    entry.said
                ));
            }
            out.push_str(
                "Changes attributed to `you` were made by the person, in the browser, \
                 not by you the agent.\n",
            );
        }

        out.push_str(&format!(
            "\nHOW TO CHANGE THE ROOM\n\
             put_pane(id, title, view, place?, size?, expected_revision) — create or \
             rewrite a pane; the same id twice rewrites in place, leaving it where the person \
             put it.\n\
             remove_pane, arrange_room (place/size/pinned), configure_room \
             (intent/theme).\n\
             Panes sit on a free canvas with no grid: `place` names a neighbour — \
             \"right of: <id>\", \"below: <id>\", \"near: <id>\" — or \"start\"/\"end\". \
             `size` is a shape: small, medium, wide, tall, large. Neither takes a number, \
             and the room is always described back to you in the same relational words.\n\
             A view is a node tree. Kinds: {VOCABULARY}.\n\
             `source` takes a repo-relative path plus optional from/to and is re-read from \
             disk on every render, so it never goes stale. `options` renders the live \
             catalog of runnable packages. `button` sends its `ask` to you as if they typed \
             it. GET /room/vocabulary has every field.\n\
             You cannot set a mark or a note — those are theirs.\n\
             await_room blocks until they touch something and then reads it back, so you \
             can answer and wait instead of asking again.\n\
             Always pass expected_revision = {}.\n",
            doc.revision
        ));

        Ok(out)
    }

    fn describe_short(&self) -> String {
        let doc = self.doc.lock();
        let mut out = format!(
            "THE ROOM (revision {}): {}\n",
            doc.revision, doc.intent
        );
        if doc.panes.is_empty() {
            out.push_str("No panes yet.\n");
            return out;
        }
        for pane in &doc.panes {
            out.push_str(&format!(
                "- [{}] “{}” by {}{}\n",
                pane.id,
                pane.title,
                credit(pane.author, pane.by_name.as_deref()),
                match pane.mark.describe() {
                    Some(mark) => format!(" · MARKED {mark}"),
                    None => String::new(),
                }
            ));
        }
        out.push_str("Call read_room for what changed since you last looked.\n");
        out
    }
}

impl SurfaceState for RoomState {
    fn backing(&self) -> StateBacking {
        StateBacking::LastWriterWins
    }

    fn describe(&self) -> Result<String, String> {
        Ok(self.describe_short())
    }

    fn snapshot(&self) -> Result<StateSnapshot, String> {
        Ok(StateSnapshot {
            backing: StateBacking::LastWriterWins,
            body: self.resolved(),
            chrome: None,
        })
    }

    fn resolve(&self, id: &str) -> Result<Option<String>, String> {
        let doc = self.doc.lock();
        let Some(pane) = doc.panes.iter().find(|pane| pane.id == id) else {
            return Ok(None);
        };
        let mut out = format!(
            "Room pane [{}] “{}”, written by {}, at room revision {}.\nContents: {}",
            pane.id,
            pane.title,
            credit(pane.author, pane.by_name.as_deref()),
            doc.revision,
            view::summarize(&pane.view),
        );
        if let Some(mark) = pane.mark.describe() {
            out.push_str(&format!("\nThey marked it {mark}"));
        }
        if !pane.note.is_empty() {
            out.push_str(&format!("\nTheir note: “{}”", pane.note));
        }
        Ok(Some(out))
    }

    fn reconnect_events(&self) -> Vec<(String, JsonValue)> {
        vec![(EVENT_NAME.to_string(), self.resolved())]
    }
}

impl SurfaceStore for RoomState {
    fn context(&self) -> String {
        self.describe_short()
    }
}

pub struct RoomExtension {
    state: Arc<RoomState>,
    actions: Vec<ToolDef>,
}

impl RoomExtension {
    pub fn new(state: Arc<RoomState>) -> Self {
        Self {
            actions: actions(state.clone()),
            state,
        }
    }
}

impl Extension for RoomExtension {
    fn id(&self) -> &str {
        "room"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn state(&self) -> &dyn SurfaceState {
        self.state.as_ref()
    }

    fn actions(&self) -> &[ToolDef] {
        &self.actions
    }

    fn client_module(&self) -> Option<ClientModule> {
        Some(
            ClientModule::lazy("room", "0.1.0", "/extensions/room/index.js", "room-view")
                .action("room_put_pane")
                .action("room_remove_pane")
                .action("room_arrange")
                .action("room_configure")
                .action("room_annotate_pane")
                .event(EVENT_NAME)
                .capability("dom")
                .capability("local-state")
                .capability("embed-web"),
        )
    }

    fn capabilities(&self) -> Vec<&str> {
        vec!["dom", "local-state", "embed-web"]
    }

    fn store(&self) -> Option<&dyn SurfaceStore> {
        Some(self.state.as_ref())
    }

    fn focus_events(&self) -> &[&str] {
        &[FOCUS_EVENT]
    }

    /// Remember who is calling so an agent-audience write gets an honest
    /// byline — this room's own agent and an outside assistant reaching in
    /// over HTTP are both models, but they are not the same author.
    fn note_caller(&self, actor: &ag_ui_surface::Actor) {
        *self.state.caller.lock() = actor.caller;
        self.state.caller_name.lock().clone_from(&actor.label);
    }

    fn routes(&self) -> Vec<RouteDef> {
        let state = self.state.clone();
        vec![
            RouteDef {
                method: HttpMethod::Get,
                path: "/room/vocabulary",
                handler: Box::new(|_request: RouteRequest| {
                    Box::pin(async move {
                        RouteResponse {
                            status: 200,
                            body: ag_ui_surface::RouteBody::Json(vocabulary_reference()),
                        }
                    })
                }),
            },
            // An `html` node cannot run as `srcdoc`: the page's own CSP
            // (`script-src 'self'`) is inherited by srcdoc children, which is
            // right for the page and fatal for the sandbox. So each html node
            // is served as its *own* document here — same origin by URL, but
            // framed with `sandbox="allow-scripts"` and no
            // `allow-same-origin`, so the running document has an opaque
            // origin and its only way out is the pointer bridge prepended
            // below.
            RouteDef {
                method: HttpMethod::Get,
                path: "/room/pane-html",
                handler: Box::new(move |request: RouteRequest| {
                    let state = state.clone();
                    Box::pin(async move {
                        let pane_id = request.query.get("pane").cloned().unwrap_or_default();
                        let index: usize = request
                            .query
                            .get("index")
                            .and_then(|value| value.parse().ok())
                            .unwrap_or(0);
                        let doc = state.doc.lock();
                        let markup = doc
                            .panes
                            .iter()
                            .find(|pane| pane.id == pane_id)
                            .and_then(|pane| {
                                let mut nodes = Vec::new();
                                view::collect_html(&pane.view, &mut nodes);
                                nodes.get(index).map(|html| (*html).to_string())
                            });
                        match markup {
                            Some(html) => RouteResponse {
                                status: 200,
                                body: ag_ui_surface::RouteBody::Html(format!(
                                    "{POINTER_BRIDGE_DOC}{html}"
                                )),
                            },
                            None => RouteResponse {
                                status: 404,
                                body: ag_ui_surface::RouteBody::Html(
                                    "<!doctype html><p>no such html node</p>".to_string(),
                                ),
                            },
                        }
                    })
                }),
            },
        ]
    }
}

/// Prepended to every served `html` node. Reports what the person clicks —
/// the nearest `data-point` label, or a text fallback — to the host page,
/// which records it as the pane's note. The agent's own script may call
/// `parent.postMessage({aguiPoint: "…"}, "*")` too, to report a composed
/// result like the set of checked boxes.
///
/// The text fallback makes pointing free for simple panes, but it also means
/// every click anywhere is a note — and a note is a room revision. A pane with
/// its own chrome (an accordion, tabs, a control panel) would write one on each
/// expand, saying nothing. `data-quiet` on any ancestor suppresses the
/// fallback inside that subtree; an explicit `data-point` still reports, so
/// opting a region out of chatter never costs a deliberate point.
const POINTER_BRIDGE_DOC: &str = concat!(
    "<!doctype html><meta charset=\"utf-8\">",
    "<style>html,body{margin:0;background:transparent}</style>",
    "<script>document.addEventListener(\"click\",(event)=>{",
    "const hit=event.target.closest(\"[data-point]\");",
    "if(!hit&&event.target.closest(\"[data-quiet]\"))return;",
    "const label=hit?hit.getAttribute(\"data-point\")",
    ":(event.target.textContent||event.target.tagName||\"\").trim().slice(0,80);",
    "if(label)parent.postMessage({aguiPoint:label},\"*\");",
    "},true);</script>"
);

/// Ten actions over six implementations. The `room_*` half is what the browser
/// module owns; the unprefixed half is what the model is shown.
fn actions(state: Arc<RoomState>) -> Vec<ToolDef> {
    let revision = json!({
        "type": "integer",
        "minimum": 0,
        "description": "The room revision this edit is based on. A stale value is rejected rather than overwriting newer work."
    });
    let id = json!({
        "type": "string",
        "pattern": "^[a-z0-9][a-z0-9_-]{0,47}$",
        "description": "Stable pane id. Reusing an id rewrites that pane in place instead of adding a second one."
    });
    let size = json!({
        "type": "string",
        "enum": ["small", "medium", "wide", "tall", "large"],
        "description": "Roughly how big this pane should be. A shape, not a measurement — the person resizes freely by dragging, and their size wins."
    });
    let place = json!({
        "type": "string",
        "description": "Where to put it, relative to another pane: \"right of: <pane-id>\", \"left of: <pane-id>\", \"below: <pane-id>\", \"above: <pane-id>\", \"near: <pane-id>\", or \"start\"/\"end\" for the top or bottom of the canvas. The room has no grid and no columns — panes sit wherever they were put, and you position yours by naming a neighbour. Coordinates are not accepted. Omit to leave an existing pane exactly where the person left it."
    });
    let theme = json!({
        "type": "object",
        "additionalProperties": false,
        "description": "Appearance as data. Only these tokens exist; the room never accepts CSS.",
        "properties": {
            "accent": { "type": "string", "pattern": "^#[0-9a-fA-F]{6}$" },
            "surface": { "type": "string", "enum": SURFACES },
            "density": { "type": "string", "enum": DENSITIES },
            "radius": { "type": "integer", "minimum": 0, "maximum": 24 },
            "scale": { "type": "number", "minimum": 0.8, "maximum": 1.4 }
        }
    });

    let put_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "expected_revision": revision,
            "id": id,
            "title": { "type": "string", "minLength": 1, "maxLength": MAX_TITLE },
            "view": {
                "type": "object",
                "description": "A node tree. Every node is {\"kind\": ...}. Kinds: stack{children}, row{children,wrap}, heading{text,level}, text{text,tone}, code{text,lang}, list{items,ordered}, kv{items:[{label,value}]}, table{columns,rows}, badge{text,tone}, divider, button{label,ask} (sends `ask` to you as if they typed it), field{key,label,placeholder,multiline} (its value is appended to any button `ask` in the same pane), link{label,url}, image{src} (same-origin or data: only), source{path,from,to} (repo-relative, re-read from disk on every render), options{filter} (the live catalog of runnable packages), embed{url,height} (a site framed in the room), diagram{nodes:[{id,label,tone}],edges:[{from,to,label,arrow}],direction,caption} (boxes and arrows — you send structure only and the host lays it out; use it whenever the point is how things connect). Tones: neutral, muted, strong, accent, good, warn, bad. GET /room/vocabulary is the full reference."
            },
            "size": size,
            "place": place
        },
        "required": ["expected_revision", "id", "title", "view"]
    });
    let remove_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": { "expected_revision": revision, "id": id },
        "required": ["expected_revision", "id"]
    });
    // Two schemas, not one with an optional field. The browser's twin accepts a
    // rectangle because a drag produces one; the agent's twin must never be
    // *shown* that such a field exists, or a model will reasonably try it and
    // spend a turn being refused. The audience split is the documentation.
    let arrange_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "expected_revision": revision,
            "panes": {
                "type": "array",
                "description": "The panes to move, resize or pin. Position is always relative to another pane.",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "id": id, "place": place, "size": size, "pinned": { "type": "boolean" } },
                    "required": ["id"]
                }
            }
        },
        "required": ["expected_revision"]
    });
    let arrange_schema_human = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "expected_revision": revision,
            "panes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "id": id,
                        "place": place,
                        "size": size,
                        "pinned": { "type": "boolean" },
                        "spot": {
                            "type": "object",
                            "additionalProperties": false,
                            "description": "The rectangle the person dragged this pane to, in canvas units. Browser-only.",
                            "properties": {
                                "x": { "type": "number" },
                                "y": { "type": "number" },
                                "w": { "type": "number" },
                                "h": { "type": "number" }
                            },
                            "required": ["x", "y", "w", "h"]
                        }
                    },
                    "required": ["id"]
                }
            }
        },
        "required": ["expected_revision"]
    });
    let configure_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "expected_revision": revision,
            "intent": { "type": "string", "minLength": 1, "maxLength": MAX_INTENT, "description": "One line naming what this room is for right now." },
            "theme": theme
        },
        "required": ["expected_revision"]
    });

    let put_description = "Create a pane, or rewrite one in place by reusing its id. This is how \
        you put anything in front of the person: prose, a source excerpt, a table, a form, a \
        site, a menu of buttons. A pane's `view` is a node tree, never markup.";
    let remove_description =
        "Take a pane down. A pinned pane is refused until the person unpins it.";
    let arrange_description = "Move a pane next to another one, change its size, or pin it. \
        Panes sit on a free canvas, so you place yours by naming a neighbour — \"right of: \
        <id>\", \"below: <id>\" — never by coordinate. The person drags panes wherever they \
        like, and read_room tells you where everything ended up in the same relational words.";
    let configure_description = "Change what the room is for and how it looks. Appearance is a \
        fixed set of tokens, not CSS. The room has no column count to set — panes sit wherever \
        they were placed on a free canvas.";

    let mut defs = Vec::new();
    for (name, audience, by) in [
        ("put_pane", ActionAudience::Agent, None),
        ("room_put_pane", ActionAudience::Human, Some(Byline::you())),
    ] {
        let state = state.clone();
        defs.push(
            ToolDef::new(name, put_description, put_schema.clone(), move |args| {
                let by = by.clone();
                effect(state.clone(), args.clone(), move |state, args| {
                    state.put_pane(by.clone().unwrap_or_else(|| state.agent_byline()), &args)
                })
            })
            .audience(audience),
        );
    }
    for (name, audience, by) in [
        ("remove_pane", ActionAudience::Agent, None),
        ("room_remove_pane", ActionAudience::Human, Some(Byline::you())),
    ] {
        let state = state.clone();
        defs.push(
            ToolDef::new(
                name,
                remove_description,
                remove_schema.clone(),
                move |args| {
                    let by = by.clone();
                    effect(state.clone(), args.clone(), move |state, args| {
                        state.remove_pane(by.clone().unwrap_or_else(|| state.agent_byline()), &args)
                    })
                },
            )
            .audience(audience),
        );
    }
    for (name, audience, by) in [
        ("arrange_room", ActionAudience::Agent, None),
        ("room_arrange", ActionAudience::Human, Some(Byline::you())),
    ] {
        let state = state.clone();
        let schema = if by.is_some() {
            arrange_schema_human.clone()
        } else {
            arrange_schema.clone()
        };
        defs.push(
            ToolDef::new(
                name,
                arrange_description,
                schema,
                move |args| {
                    let by = by.clone();
                    effect(state.clone(), args.clone(), move |state, args| {
                        // A byline handed in by the registration means this is
                        // the browser's twin, which is the only channel a
                        // rectangle may arrive on.
                        let from_person = by.is_some();
                        state.arrange(
                            by.clone().unwrap_or_else(|| state.agent_byline()),
                            from_person,
                            &args,
                        )
                    })
                },
            )
            .audience(audience),
        );
    }
    for (name, audience, by) in [
        ("configure_room", ActionAudience::Agent, None),
        ("room_configure", ActionAudience::Human, Some(Byline::you())),
    ] {
        let state = state.clone();
        defs.push(
            ToolDef::new(
                name,
                configure_description,
                configure_schema.clone(),
                move |args| {
                    let by = by.clone();
                    effect(state.clone(), args.clone(), move |state, args| {
                        state.configure(by.clone().unwrap_or_else(|| state.agent_byline()), &args)
                    })
                },
            )
            .audience(audience),
        );
    }

    defs.push(
        ToolDef::new(
            "await_room",
            "Wait until the person touches the room, then read it. Blocks until someone other \
             than you marks, notes, moves or rewrites something, and returns exactly what \
             read_room would. Use it to stay on the page with them instead of asking again: \
             answer, then wait. Returns after `seconds` (default 60, max 600) with nothing to \
             report if the room stayed still — a quiet room is not an error, just call it again.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "seconds": {
                        "type": "integer", "minimum": 1, "maximum": MAX_WAIT_SECONDS,
                        "description": "How long to wait before returning empty-handed."
                    }
                }
            }),
            {
                let state = state.clone();
                move |args| {
                    let state = state.clone();
                    let seconds = match optional_u64(args, "seconds") {
                        Ok(value) => value.unwrap_or(DEFAULT_WAIT_SECONDS),
                        Err(error) => return Effect::Reject(error),
                    };
                    if !(1..=MAX_WAIT_SECONDS).contains(&seconds) {
                        return Effect::Reject(format!(
                            "seconds must be between 1 and {MAX_WAIT_SECONDS}"
                        ));
                    }
                    // The byline is read now, on the calling thread, because
                    // `note_caller` reflects whoever called most recently — by
                    // the time this future wakes, another caller may have
                    // arrived and moved it.
                    let me = state.agent_byline();
                    Effect::AsyncQuery(Box::pin(async move {
                        let deadline =
                            tokio::time::Instant::now() + Duration::from_secs(seconds);
                        loop {
                            // Register interest *before* looking, or a change
                            // landing between the check and the wait is a
                            // wakeup nobody receives.
                            let woken = state.changed.notified();
                            if state.unread_from_others(&me) {
                                return state.read();
                            }
                            tokio::select! {
                                _ = woken => {}
                                _ = tokio::time::sleep_until(deadline) => {
                                    return Ok(format!(
                                        "Waited {seconds}s and the room did not change. \
                                         Nobody else has touched it since your last read. \
                                         Call await_room again to keep waiting."
                                    ));
                                }
                            }
                        }
                    }))
                }
            },
        )
        .agent_only(),
    );

    defs.push(
        ToolDef::new(
            "read_room",
            "Read the whole room: what is on the page, what the person marked, and — the part \
             to act on — what they changed since your last read. Call this at the start of \
             every turn, before you write anything.",
            json!({ "type": "object", "additionalProperties": false, "properties": {} }),
            {
                let state = state.clone();
                move |_args| {
                    let state = state.clone();
                    Effect::Query(Box::new(move |_surface| state.read()))
                }
            },
        )
        .agent_only(),
    );

    defs.push(
        ToolDef::new(
            "room_annotate_pane",
            "Mark a pane, or leave a note on it. This action exists only for the person; it is \
             absent from the agent's catalog so a mark always means the human put it there.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "expected_revision": revision,
                    "id": id,
                    "mark": { "type": "string", "enum": ["none", "question", "important", "agree", "disagree"] },
                    "note": { "type": "string", "maxLength": MAX_NOTE }
                },
                "required": ["expected_revision", "id"]
            }),
            {
                let state = state.clone();
                move |args| {
                    effect(state.clone(), args.clone(), move |state, args| {
                        state.annotate(&args)
                    })
                }
            },
        )
        .human_only(),
    );

    defs
}

fn effect<F>(state: Arc<RoomState>, args: JsonValue, run: F) -> Effect
where
    F: FnOnce(Arc<RoomState>, JsonValue) -> Result<Option<String>, String> + Send + 'static,
{
    Effect::Mutate(Box::new(move |_surface| run(state, args)))
}

/// One pane's worth of change requested through `arrange`.
struct Adjustment {
    id: String,
    place: Option<Placement>,
    size: Option<(f64, f64)>,
    /// Only ever `Some` on the human-audience twin — see [`RoomState::arrange`].
    spot: Option<Spot>,
    pinned: Option<bool>,
}

/// What the agent is told when it tries to send a rectangle.
///
/// Worth the words: a refusal that only says "not allowed" invites the model to
/// retry the same shape, whereas naming the vocabulary it *should* be using
/// turns the refusal into the documentation.
const SPOT_IS_NOT_YOURS: &str = "arrange_room does not take coordinates, and it never will — \
     the room is described to you in relations so you and the person can talk about it in the \
     same words. Say where a pane should go relative to another one: place: \"right of: <id>\", \
     \"left of: <id>\", \"below: <id>\", \"above: <id>\", \"near: <id>\", start, or end. For \
     size, name a shape: small, medium, wide, tall or large. Dragging is the person's; a \
     rectangle only ever reaches the room from their pointer.";

/// The named shapes an agent may ask for. Deliberately coarse: an agent choosing
/// between "wide" and "tall" is making a judgement about content, which is its
/// business; an agent choosing between 840 and 860 pixels is doing the person's
/// job badly.
fn parse_size(value: &str) -> Result<(f64, f64), String> {
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
fn parse_spot(raw: &JsonValue) -> Result<Spot, String> {
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
fn migrate(body: &mut JsonValue) -> Result<(), String> {
    let version = body
        .get("schema_version")
        .and_then(JsonValue::as_u64)
        .unwrap_or(1);
    if version >= SCHEMA_VERSION as u64 {
        return Ok(());
    }

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
    body["schema_version"] = json!(SCHEMA_VERSION);
    Ok(())
}

fn merge_theme(current: &Theme, args: &JsonValue) -> Result<Theme, String> {
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
    if let Some(scale) = args.get("scale") {
        if !scale.is_null() {
            let scale = scale
                .as_f64()
                .ok_or_else(|| "scale must be a number".to_string())?;
            if !(0.8..=1.4).contains(&scale) {
                return Err("scale must be between 0.8 and 1.4".to_string());
            }
            theme.scale = (scale * 100.0).round() / 100.0;
        }
    }
    Ok(theme)
}

fn validate_doc(doc: &RoomDoc) -> Result<(), String> {
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

fn vocabulary_reference() -> JsonValue {
    json!({
        "note": "The complete room view vocabulary. A pane's `view` is one node; containers nest \
                 up to 8 deep, with at most 400 nodes in a tree. Text is rendered as text — the \
                 renderer never parses markup.",
        "tones": ["neutral", "muted", "strong", "accent", "good", "warn", "bad"],
        "nodes": {
            "stack": { "children": "[node]", "gap": "0-24, optional" },
            "row": { "children": "[node]", "gap": "0-24, optional", "wrap": "bool, default true" },
            "deck": { "children": "[node] — one shown at a time", "titles": "[string], optional — one per child, labels the flip counter", "note": "For a sequence the person flips through — review steps, alternatives — when showing everything at once would spend the whole screen. Flipping is local to each reader and never round-trips through the agent." },
            "heading": { "text": "string", "level": "1-3, optional" },
            "text": { "text": "string", "tone": "tone, optional" },
            "code": { "text": "string", "lang": "label only; nothing is highlighted or run" },
            "list": { "items": "[string]", "ordered": "bool" },
            "kv": { "items": "[{label, value}]" },
            "table": { "columns": "[string]", "rows": "[[string]] — every row must match the column count" },
            "badge": { "text": "string", "tone": "tone, optional" },
            "divider": {},
            "button": { "label": "string", "ask": "sent to the agent as if the person typed it", "tone": "tone, optional" },
            "field": { "key": "string", "label": "optional", "placeholder": "optional", "multiline": "bool", "note": "values are appended to any button `ask` in the same pane" },
            "link": { "label": "string", "url": "http(s) only" },
            "image": { "src": "same-origin absolute path or data:image/ URI", "alt": "optional" },
            "source": { "path": "repository-relative", "from": "1-based, optional", "to": "optional", "note": "re-read from disk on every render, so it cannot go stale" },
            "options": { "filter": "optional substring", "note": "renders the live catalog of runnable packages, with a live port probe" },
            "embed": { "url": "http(s)", "height": "120-2000 px", "note": "sandboxed; an agent-authored embed does not load until the person clicks it" },
            "html": { "html": "raw HTML, max 48000 chars", "height": "120-2000 px, default 320", "note": "The no-build escape hatch: any interface the vocabulary lacks — checkboxes, a canvas experiment, a control panel — authored directly, rendered in a fully isolated sandbox (scripts run inside, nothing reaches the page). Put data-point=\"label\" on elements that matter: when the person clicks one, the label becomes the pane's note, so read_room tells you exactly what they touched. Your own script may also call parent.postMessage({aguiPoint: \"label\"}, \"*\") to report a composed result, e.g. every checked box. Any click without a data-point reports its own text instead, which is free for a simple pane but chatty for one with its own chrome — put data-quiet on a wrapper to silence that subtree; an explicit data-point inside it still reports." },
            "diagram": {
                "nodes": "[{id, label optional (defaults to id), tone optional}]",
                "edges": "[{from, to — node ids; label optional; arrow bool, default true}]",
                "direction": "\"right\" (roots left, flow rightward — pipelines, computation graphs) or \"down\" (roots on top). Default \"right\".",
                "caption": "optional line under the drawing",
                "note": "Send STRUCTURE only — never coordinates. The host lays it out: boxes sized to their labels, arrows landing on box edges, layers spaced, nothing overlapping or off-frame. Prefer this over prose or a table whenever the point is how things connect. Clicking a box asks about that node, so the person can point at one part of the picture."
            }
        },
        "refused": [
            "HTML, CSS, JavaScript or event handlers in any field",
            "remote image sources",
            "javascript:, data: or credentialed URLs in link and embed",
            "paths outside the project root, and anything under .git, .local, target, node_modules or pkg",
            "files that look like credentials, and non-text file types"
        ]
    })
}

fn load(path: &std::path::Path) -> Result<RoomDoc, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("could not stat {}: {error}", path.display()))?;
    if metadata.len() > MAX_STATE_BYTES {
        return Err(format!(
            "{} is larger than the {MAX_STATE_BYTES}-byte room limit",
            path.display()
        ));
    }
    let body = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let mut raw: JsonValue = serde_json::from_str(&body)
        .map_err(|error| format!("{} is not valid JSON: {error}", path.display()))?;
    let before = raw.get("schema_version").and_then(JsonValue::as_u64);
    migrate(&mut raw)?;
    let migrated = before != Some(SCHEMA_VERSION as u64);
    let doc: RoomDoc = serde_json::from_value(raw)
        .map_err(|error| format!("{} is not a valid room document: {error}", path.display()))?;
    validate_doc(&doc)?;
    if migrated {
        // Write the upgraded shape back now rather than on the next mutation,
        // so a room that is only ever read does not get migrated afresh every
        // time it opens.
        persist(path, &doc)?;
    }
    Ok(doc)
}

fn persist(path: &std::path::Path, doc: &RoomDoc) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let body = serde_json::to_vec_pretty(doc)
        .map_err(|error| format!("could not serialize the room: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, &body)
        .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, path)
        .map_err(|error| format!("could not replace {}: {error}", path.display()))?;
    Ok(())
}

fn pane_id(value: &str) -> Result<String, String> {
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

fn checked(field: &str, value: &str, minimum: usize, maximum: usize) -> Result<String, String> {
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

fn required_u64(args: &JsonValue, field: &str) -> Result<u64, String> {
    args.get(field)
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| format!("{field} must be a non-negative integer"))
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

fn optional_bool(args: &JsonValue, field: &str) -> Result<Option<bool>, String> {
    match args.get(field) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("{field} must be a boolean when supplied")),
    }
}

fn required_string(args: &JsonValue, field: &str) -> Result<String, String> {
    args.get(field)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("{field} must be a string"))
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use ag_ui_surface::{ActionAudience, AsyncQueryEffect};

    use super::*;

    /// `VOCABULARY` is the one line an agent actually reads — it is quoted in
    /// `read_room` and in the error a bad view gets back. `vocabulary_reference()`
    /// is the long form behind `GET /room/vocabulary`. A kind that reaches one and
    /// not the other is a read-back that lies: `deck` and `html` both shipped
    /// missing from the short list, so an attached agent could not learn they
    /// existed and was told they were invalid if it guessed.
    #[test]
    fn the_short_vocabulary_lists_exactly_the_documented_kinds() {
        let reference = vocabulary_reference();
        let mut documented: Vec<&str> = reference["nodes"]
            .as_object()
            .expect("vocabulary reference has a nodes object")
            .keys()
            .map(String::as_str)
            .collect();
        documented.sort_unstable();

        let mut listed: Vec<&str> = VOCABULARY.split(',').map(str::trim).collect();
        assert!(
            listed.iter().all(|kind| !kind.is_empty()),
            "VOCABULARY has an empty entry: {VOCABULARY}"
        );
        listed.sort_unstable();

        assert_eq!(
            listed, documented,
            "VOCABULARY and vocabulary_reference() disagree. Every node kind needs \
             a line in both — see examples/same-page-room/AGENTS.md."
        );
    }

    fn test_transport() -> Transport {
        let (ws_tx, _) = tokio::sync::broadcast::channel(32);
        let (sse_tx, _) = tokio::sync::broadcast::channel(32);
        Transport {
            ws_tx,
            sse_tx,
            history: Arc::new(Mutex::new(VecDeque::new())),
            awaiting: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            transcript_replay_lock: Arc::new(Mutex::new(())),
        }
    }

    fn fresh(name: &str) -> (Arc<RoomState>, RoomExtension) {
        let path = std::env::temp_dir().join(format!(
            "same-page-room-test-{}-{name}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("workspace root");
        let workspace = Arc::new(Workspace::open(root).expect("workspace opens"));
        let state = RoomState::open(test_transport(), path, workspace).expect("room opens");
        let extension = RoomExtension::new(state.clone());
        (state, extension)
    }

    fn action<'a>(extension: &'a RoomExtension, name: &str) -> &'a ToolDef {
        extension
            .actions()
            .iter()
            .find(|action| action.name == name)
            .unwrap_or_else(|| panic!("action {name} must exist"))
    }

    #[test]
    fn a_companion_gets_its_own_byline_rather_than_the_persons() {
        let (state, extension) = fresh("companion-byline");

        // The default: this room's own agent writes a pane.
        call(
            &extension,
            &state,
            "put_pane",
            json!({
                "expected_revision": state.doc.lock().revision,
                "id": "by-agent",
                "title": "from the room agent",
                "view": { "kind": "text", "text": "hello" }
            }),
        )
        .expect("the room agent writes a pane");

        // The same action, reached by an assistant driving the room over HTTP.
        extension.note_caller(&ag_ui_surface::Actor::anonymous(Caller::Companion));
        call(
            &extension,
            &state,
            "put_pane",
            json!({
                "expected_revision": state.doc.lock().revision,
                "id": "by-companion",
                "title": "from the terminal",
                "view": { "kind": "text", "text": "hello" }
            }),
        )
        .expect("a companion writes a pane");

        let doc = state.doc.lock();
        let author = |id: &str| {
            doc.panes
                .iter()
                .find(|pane| pane.id == id)
                .unwrap_or_else(|| panic!("pane {id}"))
                .author
        };
        assert_eq!(author("by-agent"), Author::Agent);
        assert_eq!(
            author("by-companion"),
            Author::Companion,
            "a companion's pane must not be signed by this room's agent"
        );
        // And never by the person — that is the failure this exists to stop.
        assert!(doc.panes.iter().all(|pane| pane.author != Author::You));
        assert!(doc.log.iter().any(|entry| entry.by == Author::Companion));
    }

    #[test]
    fn a_companion_may_run_agent_actions_but_a_human_still_may_not() {
        // The authorisation half. A companion is a model, so it gets a model's
        // permissions — otherwise driving the room over HTTP means being
        // refused every action worth calling.
        assert!(Caller::Companion.may_call(ActionAudience::Agent));
        assert!(Caller::Companion.may_call(ActionAudience::Both));
        assert!(!Caller::Companion.may_call(ActionAudience::Human));
        assert!(!Caller::Human.may_call(ActionAudience::Agent));
    }

    #[test]
    fn an_unrecognised_caller_falls_back_to_the_least_privilege() {
        assert_eq!(Caller::parse(Some("companion")), Caller::Companion);
        assert_eq!(Caller::parse(Some("COMPANION ")), Caller::Companion);
        assert_eq!(Caller::parse(None), Caller::Human);
        // Least privilege used to mean falling back to the human's browser
        // permissions; the runtime now parses a self-claimed "agent" or a
        // misspelled identity to Unknown, which may call nothing at all.
        assert_eq!(Caller::parse(Some("agent")), Caller::Unknown);
        assert_eq!(Caller::parse(Some("nonsense")), Caller::Unknown);
        assert!(!Caller::Unknown.may_call(ActionAudience::Agent));
        assert!(!Caller::Unknown.may_call(ActionAudience::Human));
        assert!(!Caller::Unknown.may_call(ActionAudience::Both));
    }

    fn call(
        extension: &RoomExtension,
        state: &RoomState,
        name: &str,
        args: JsonValue,
    ) -> Result<Option<String>, String> {
        match (action(extension, name).apply)(&args) {
            Effect::Mutate(apply) => apply(state),
            Effect::Query(query) => query(state).map(Some),
            Effect::Reject(error) => Err(error),
            _ => panic!("unexpected effect from {name}"),
        }
    }

    fn revision(state: &RoomState) -> u64 {
        state.doc.lock().revision
    }

    fn text_view(text: &str) -> JsonValue {
        json!({ "kind": "stack", "children": [{ "kind": "text", "text": text }] })
    }

    #[test]
    fn the_agent_cannot_reach_the_actions_that_record_the_person() {
        let (_state, extension) = fresh("audience");
        let agent: Vec<&str> = extension
            .actions()
            .iter()
            .filter(|action| {
                matches!(
                    action.audience,
                    ActionAudience::Agent | ActionAudience::Both
                )
            })
            .map(|action| action.name.as_str())
            .collect();
        for human_only in [
            "room_put_pane",
            "room_remove_pane",
            "room_arrange",
            "room_configure",
            "room_annotate_pane",
        ] {
            assert!(
                !agent.contains(&human_only),
                "{human_only} must be absent from the agent's catalog"
            );
        }
        assert!(agent.contains(&"put_pane") && agent.contains(&"read_room"));
        // The mark channel has no agent twin at all, under any name.
        assert!(!agent.iter().any(|name| name.contains("annotate")));
    }

    /// `await_room` hands back a future rather than a value, so it needs its
    /// own driver; `call` panics on anything but the synchronous effects.
    fn wait_effect(extension: &RoomExtension, seconds: u64) -> AsyncQueryEffect {
        match (action(extension, "await_room").apply)(&json!({ "seconds": seconds })) {
            Effect::AsyncQuery(future) => future,
            _ => panic!("await_room should be an async query"),
        }
    }

    #[tokio::test]
    async fn await_room_returns_at_once_when_the_person_already_wrote_something() {
        let (state, extension) = fresh("await-already");
        call(
            &extension,
            &state,
            "put_pane",
            json!({ "expected_revision": revision(&state), "id": "topic", "title": "T", "view": text_view("draft") }),
        )
        .expect("agent writes");
        state.read().expect("the agent catches up");
        call(
            &extension,
            &state,
            "room_annotate_pane",
            json!({ "expected_revision": revision(&state), "id": "topic", "note": "this one" }),
        )
        .expect("the person notes");

        let seen = wait_effect(&extension, 5).await.expect("the wait returns");
        assert!(
            seen.contains("this one"),
            "an unread human change must not be made to wait: {seen}"
        );
    }

    /// The point of the whole thing: the agent is parked when the person
    /// writes, and finds out without asking.
    #[tokio::test]
    async fn await_room_wakes_when_the_person_writes_while_it_is_parked() {
        let (state, extension) = fresh("await-wakes");
        call(
            &extension,
            &state,
            "put_pane",
            json!({ "expected_revision": revision(&state), "id": "topic", "title": "T", "view": text_view("draft") }),
        )
        .expect("agent writes");
        state.read().expect("the agent catches up");

        let waiting = tokio::spawn(wait_effect(&extension, 30));
        // Let the wait actually park before anything changes, so this proves a
        // wakeup rather than the already-unread path above.
        tokio::time::sleep(Duration::from_millis(150)).await;

        call(
            &extension,
            &state,
            "room_annotate_pane",
            json!({ "expected_revision": revision(&state), "id": "topic", "mark": "question", "note": "while you were waiting" }),
        )
        .expect("the person notes");

        let seen = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("the wait should wake well inside its 30s budget")
            .expect("the wait task did not panic")
            .expect("the wait returns a read-back");
        assert!(
            seen.contains("while you were waiting"),
            "the wakeup must carry what they wrote: {seen}"
        );
    }

    #[tokio::test]
    async fn await_room_does_not_wake_the_agent_for_its_own_writing() {
        let (state, extension) = fresh("await-self");
        state.read().expect("the agent catches up");
        call(
            &extension,
            &state,
            "put_pane",
            json!({ "expected_revision": revision(&state), "id": "mine", "title": "Mine", "view": text_view("hello") }),
        )
        .expect("the agent writes its own pane");

        let seen = wait_effect(&extension, 1).await.expect("the wait returns");
        assert!(
            seen.contains("did not change"),
            "read → write → wait must not spin on the agent's own change: {seen}"
        );
    }

    #[tokio::test]
    async fn a_quiet_room_is_reported_not_raised_as_an_error() {
        let (_state, extension) = fresh("await-quiet");
        let seen = wait_effect(&extension, 1)
            .await
            .expect("a quiet room is not a failure");
        assert!(seen.contains("did not change"), "{seen}");
        assert!(
            seen.contains("await_room again"),
            "the agent needs to be told to keep waiting: {seen}"
        );
    }

    /// The bridge reports the text of anything clicked that carries no
    /// `data-point`, which makes pointing free but turns a pane's own chrome
    /// into note traffic — every accordion row expanded would write one. The
    /// opt-out has to leave deliberate points working, or it just trades one
    /// broken half for the other.
    #[test]
    fn the_pointer_bridge_can_be_kept_quiet_without_losing_deliberate_points() {
        assert!(
            POINTER_BRIDGE_DOC.contains(r#"if(!hit&&event.target.closest("[data-quiet]"))return;"#),
            "the quiet opt-out is missing from the bridge"
        );
        // Order matters: the `data-point` lookup happens first, so the early
        // return can only ever swallow the fallback.
        let hit_at = POINTER_BRIDGE_DOC
            .find("const hit=")
            .expect("the bridge still looks for a data-point");
        let quiet_at = POINTER_BRIDGE_DOC
            .find("data-quiet")
            .expect("the bridge still honours data-quiet");
        let fallback_at = POINTER_BRIDGE_DOC
            .find("textContent")
            .expect("the bridge still has a text fallback");
        assert!(
            hit_at < quiet_at && quiet_at < fallback_at,
            "data-quiet must sit between the data-point lookup and the text \
             fallback, or it would suppress explicit points too"
        );

        let reference = vocabulary_reference();
        assert!(
            reference["nodes"]["html"]
                .to_string()
                .contains("data-quiet"),
            "an escape hatch the agent is never told about is not an escape hatch"
        );
    }

    /// A tripwire, not a behaviour test — the renderer is JS and this crate has
    /// no DOM harness, so all this can do is notice if the mechanism is removed.
    ///
    /// The bug it stands guard over: a click inside an `html` pane becomes that
    /// pane's note, a note is a room change, and a room change used to re-render
    /// every pane and reload every iframe. Pointing at a sandbox destroyed the
    /// sandbox — reported live as "every time I click reform it switches right
    /// back to swirl". Two independent causes, so two assertions.
    #[test]
    fn the_renderer_still_refuses_to_reload_a_sandbox_when_a_note_lands() {
        let source = include_str!("../static/extensions/room/index.js");

        let frame_url = source
            .lines()
            .find(|line| line.contains("/room/pane-html?"))
            .expect("the renderer still builds a pane-html url");
        assert!(
            frame_url.contains("contentKey("),
            "the html frame's url must be keyed to its content: {frame_url}"
        );
        assert!(
            !frame_url.contains("rev"),
            "the html frame's url must not carry the room revision — every note \
             would change it and reload the sandbox that was just pointed at: {frame_url}"
        );

        assert!(
            source.contains("article.__update(pane)") && source.contains("panes.set("),
            "panes must be reconciled by id and refreshed in place; rebuilding \
             them reloads every iframe in the room"
        );
    }

    /// An actor that announced a name, as `/mcp` attachment produces.
    fn named_actor(caller: Caller, label: &str) -> ag_ui_surface::Actor {
        ag_ui_surface::Actor {
            caller,
            label: Some(label.to_string()),
            participant_id: Some("p-test".to_string()),
        }
    }

    #[test]
    fn an_attached_agent_signs_its_panes_with_the_name_it_announced() {
        let (state, extension) = fresh("byline-named");
        extension.note_caller(&named_actor(Caller::Agent, "Claude Code"));
        call(
            &extension,
            &state,
            "put_pane",
            json!({ "expected_revision": revision(&state), "id": "signed", "title": "Mine", "view": text_view("hello") }),
        )
        .expect("the attached agent writes a pane");

        let doc = state.doc.lock();
        let pane = doc.panes.iter().find(|p| p.id == "signed").expect("pane");
        assert_eq!(pane.by_name.as_deref(), Some("Claude Code"));
        assert_eq!(
            pane.author,
            Author::Agent,
            "the name decorates the category, it does not replace it"
        );
        assert_eq!(
            doc.log.last().expect("log entry").by_name.as_deref(),
            Some("Claude Code"),
            "the delta names the writer too, or the person cannot tell two agents apart"
        );
    }

    /// `resolved()` hand-writes the pane's wire shape field by field, so a new
    /// pane field reaches the server's own read-back and the browser by two
    /// different routes. The byline shipped down one and not the other: tests
    /// and `read_room` both said "Claude Code" while the page still rendered
    /// "AGENT".
    #[test]
    fn the_wire_shape_carries_the_byline_the_read_back_reports() {
        let (state, extension) = fresh("byline-wire");
        extension.note_caller(&named_actor(Caller::Agent, "Claude Code"));
        call(
            &extension,
            &state,
            "put_pane",
            json!({ "expected_revision": revision(&state), "id": "wired", "title": "Mine", "view": text_view("hello") }),
        )
        .expect("the attached agent writes a pane");

        let wire = state.resolved();
        let pane = wire["panes"]
            .as_array()
            .expect("panes")
            .iter()
            .find(|pane| pane["id"] == "wired")
            .expect("the pane is on the wire");
        assert_eq!(
            pane["by_name"], "Claude Code",
            "the browser renders from this shape; a byline missing here is a byline nobody sees"
        );
        assert_eq!(pane["author"], "agent");
    }

    /// The one thing a byline must never do. `Author` comes from the transport;
    /// the name is whatever the caller typed into `clientInfo.name`. An agent
    /// that calls itself "you" still has to render as an agent, or the room's
    /// central claim — that the person can tell their own writing from a
    /// machine's — is forgeable by picking a name.
    #[test]
    fn a_chosen_name_cannot_promote_a_write_into_the_persons_own() {
        let (state, extension) = fresh("byline-forge");
        extension.note_caller(&named_actor(Caller::Companion, "you"));
        call(
            &extension,
            &state,
            "put_pane",
            json!({ "expected_revision": revision(&state), "id": "forged", "title": "Not yours", "view": text_view("hello") }),
        )
        .expect("the companion writes a pane");

        let doc = state.doc.lock();
        let pane = doc.panes.iter().find(|p| p.id == "forged").expect("pane");
        assert_eq!(
            pane.author,
            Author::Companion,
            "a companion calling itself “you” is still a companion"
        );
        assert_ne!(pane.author, Author::You);
        assert_eq!(
            credit(pane.author, pane.by_name.as_deref()),
            "companion “you”",
            "the read-back leads with the category the host stands behind"
        );
    }

    #[test]
    fn an_unusable_name_falls_back_to_the_category_instead_of_refusing() {
        for label in ["   ", "a".repeat(MAX_BYLINE + 1).as_str(), "bad\nname"] {
            let byline = Byline::named(Author::Agent, Some(label.to_string()));
            assert_eq!(byline.name, None, "{label:?} should have been dropped");
            assert_eq!(byline.author, Author::Agent);
        }
        assert_eq!(
            Byline::named(Author::Agent, Some("  Codex  ".to_string())).name,
            Some("Codex".to_string()),
            "a usable name is kept, trimmed"
        );
    }

    #[test]
    fn a_room_written_before_bylines_existed_still_opens() {
        // The persisted shape gained `by_name` after panes were already on
        // disk, and later traded `span`/`height` for a rectangle. A document
        // written before either has to load, not fail closed — this is the
        // room Mike already has open.
        let mut older = json!({
            "schema_version": 1,
            "revision": 3,
            "intent": "an older room",
            "columns": 2,
            "theme": Theme::default(),
            "log": [],
            "panes": [
                {
                    "id": "old", "title": "Written earlier", "author": "agent",
                    "view": { "kind": "text", "text": "hello" },
                    "span": 1, "height": "auto", "revision": 3
                },
                {
                    "id": "wide", "title": "A wide one", "author": "agent",
                    "view": { "kind": "text", "text": "hello" },
                    "span": 2, "height": "tall", "revision": 3,
                    "mark": "important", "note": "theirs"
                }
            ]
        });
        migrate(&mut older).expect("an older document migrates");
        let doc: RoomDoc = serde_json::from_value(older).expect("and then deserializes");

        assert_eq!(doc.schema_version, SCHEMA_VERSION);
        assert_eq!(doc.panes[0].by_name, None);
        assert_eq!(credit(doc.panes[0].author, None), "agent");
        // What the person left on a pane outlives the shape change.
        assert_eq!(doc.panes[1].mark, Mark::Important);
        assert_eq!(doc.panes[1].note, "theirs");
        // The old flow is rebuilt rather than piled at the origin.
        assert!(
            doc.panes[1].spot.w > doc.panes[0].spot.w,
            "the span-2 pane should still be the wider one"
        );
        assert!(
            doc.panes.iter().all(|pane| pane.spot.w >= layout::MIN_W),
            "every migrated pane lands at a usable size"
        );
    }

    #[test]
    fn migrating_the_same_document_twice_changes_nothing() {
        let mut doc = json!({
            "schema_version": 1, "revision": 0, "intent": "x", "columns": 2,
            "theme": Theme::default(), "log": [],
            "panes": [{
                "id": "a", "title": "A", "author": "agent",
                "view": { "kind": "text", "text": "hello" },
                "span": 1, "height": "auto", "revision": 0
            }]
        });
        migrate(&mut doc).expect("first migration");
        let once = doc.clone();
        migrate(&mut doc).expect("second migration is a no-op");
        assert_eq!(once, doc, "a migrated document must not drift on reopen");
    }

    #[test]
    fn a_rewrite_keeps_the_mark_and_note_the_person_left() {
        let (state, extension) = fresh("mark-survives");
        call(
            &extension,
            &state,
            "put_pane",
            json!({ "expected_revision": revision(&state), "id": "topic", "title": "First", "view": text_view("draft") }),
        )
        .expect("agent puts a pane up");
        call(
            &extension,
            &state,
            "room_annotate_pane",
            json!({ "expected_revision": revision(&state), "id": "topic", "mark": "question", "note": "which retry?" }),
        )
        .expect("the person marks it");
        call(
            &extension,
            &state,
            "put_pane",
            json!({ "expected_revision": revision(&state), "id": "topic", "title": "Second", "view": text_view("answer") }),
        )
        .expect("the agent rewrites it");

        let doc = state.doc.lock();
        let pane = doc
            .panes
            .iter()
            .find(|pane| pane.id == "topic")
            .expect("pane");
        assert_eq!(pane.title, "Second");
        assert_eq!(
            pane.mark,
            Mark::Question,
            "the question must outlive the answer"
        );
        assert_eq!(pane.note, "which retry?");
        assert_eq!(
            doc.panes.len(),
            3,
            "reusing an id must not add a second pane"
        );
    }

    #[test]
    fn a_stale_revision_is_refused() {
        let (state, extension) = fresh("stale");
        let stale = revision(&state);
        call(
            &extension,
            &state,
            "put_pane",
            json!({ "expected_revision": stale, "id": "one", "title": "One", "view": text_view("a") }),
        )
        .expect("first write lands");
        let error = call(
            &extension,
            &state,
            "put_pane",
            json!({ "expected_revision": stale, "id": "two", "title": "Two", "view": text_view("b") }),
        )
        .expect_err("the second write is based on a stale revision");
        assert!(error.contains("revision conflict"), "{error}");
    }

    #[test]
    fn the_delta_reports_what_the_person_did_and_then_goes_quiet() {
        let (state, extension) = fresh("delta");
        state.read().expect("first read establishes the cursor");
        call(
            &extension,
            &state,
            "room_annotate_pane",
            json!({ "expected_revision": revision(&state), "id": "start", "mark": "disagree" }),
        )
        .expect("the person disagrees with a pane");

        let after = state.read().expect("second read");
        assert!(after.contains("CHANGED SINCE YOUR LAST READ"));
        assert!(after.contains("you marked"), "{after}");
        assert!(after.contains("they think this is wrong"), "{after}");

        let again = state.read().expect("third read");
        assert!(
            again.contains("Nothing. The room is exactly as you left it."),
            "{again}"
        );
    }

    #[test]
    fn read_back_expands_the_catalog_only_when_it_is_on_the_page() {
        let (state, extension) = fresh("catalog-readback");
        assert!(state.read().expect("read").contains("WHAT IS RUNNABLE"));
        call(
            &extension,
            &state,
            "remove_pane",
            json!({ "expected_revision": revision(&state), "id": "options" }),
        )
        .expect("take the options pane down");
        assert!(!state.read().expect("read").contains("WHAT IS RUNNABLE"));
    }

    #[test]
    fn a_pinned_pane_cannot_be_removed() {
        let (state, extension) = fresh("pinned");
        call(
            &extension,
            &state,
            "room_arrange",
            json!({ "expected_revision": revision(&state), "panes": [{ "id": "start", "pinned": true }] }),
        )
        .expect("the person pins it");
        let error = call(
            &extension,
            &state,
            "remove_pane",
            json!({ "expected_revision": revision(&state), "id": "start" }),
        )
        .expect_err("a pinned pane is protected");
        assert!(error.contains("pinned"), "{error}");
    }

    /// Where a pane actually ended up, for tests that care about geometry.
    fn spot_of(state: &Arc<RoomState>, id: &str) -> Spot {
        state
            .doc
            .lock()
            .panes
            .iter()
            .find(|pane| pane.id == id)
            .unwrap_or_else(|| panic!("no pane {id}"))
            .spot
    }

    #[test]
    fn place_puts_a_pane_where_it_was_asked_relative_to_another() {
        let (state, extension) = fresh("place");
        call(
            &extension,
            &state,
            "put_pane",
            json!({ "expected_revision": revision(&state), "id": "middle", "title": "Middle", "view": text_view("m"), "place": "below: start" }),
        )
        .expect("place below");
        let start = spot_of(&state, "start");
        let middle = spot_of(&state, "middle");
        assert!(
            middle.y >= start.bottom(),
            "asked to sit below start, landed at the same height"
        );
        assert!(
            (middle.x - start.x).abs() < 1.0,
            "a pane placed below another lines up with it"
        );

        call(
            &extension,
            &state,
            "arrange_room",
            json!({ "expected_revision": revision(&state), "panes": [{ "id": "middle", "place": "right of: options" }] }),
        )
        .expect("move it beside the options pane");
        let options = spot_of(&state, "options");
        let middle = spot_of(&state, "middle");
        assert!(
            middle.x >= options.right(),
            "asked to sit right of options, landed left of it"
        );
    }

    #[test]
    fn a_rewrite_leaves_a_pane_where_the_person_dragged_it() {
        let (state, extension) = fresh("stays-put");
        // The person drags it somewhere deliberate.
        call(
            &extension,
            &state,
            "room_arrange",
            json!({
                "expected_revision": revision(&state),
                "panes": [{ "id": "start", "spot": { "x": 900.0, "y": 640.0, "w": 500.0, "h": 300.0 } }]
            }),
        )
        .expect("the person moves a pane");
        let dragged = spot_of(&state, "start");

        // The agent rewrites that pane's contents without saying where it goes.
        call(
            &extension,
            &state,
            "put_pane",
            json!({ "expected_revision": revision(&state), "id": "start", "title": "Rewritten", "view": text_view("new") }),
        )
        .expect("agent rewrites it");

        assert_eq!(
            spot_of(&state, "start"),
            dragged,
            "rewriting a pane's contents must not undo where the person put it"
        );
    }

    #[test]
    fn the_agent_cannot_send_a_rectangle() {
        let (state, extension) = fresh("no-pixels");
        let error = call(
            &extension,
            &state,
            "arrange_room",
            json!({
                "expected_revision": revision(&state),
                "panes": [{ "id": "start", "spot": { "x": 10.0, "y": 10.0, "w": 400.0, "h": 300.0 } }]
            }),
        )
        .expect_err("an agent may not place by coordinate");
        assert!(error.contains("does not take coordinates"), "{error}");
        // The refusal has to teach the vocabulary that does work.
        assert!(error.contains("right of"), "{error}");

        // Belt and braces, and the layer that actually fires in production:
        // `spot` is not in the agent's schema at all, so a model is never shown
        // the field and never spends a turn being refused for trying it. This
        // call bypasses schema validation, which is why the check above alone
        // would be false comfort.
        let defs = actions(state.clone());
        let schema_of = |name: &str| {
            defs.iter()
                .find(|def| def.name == name)
                .unwrap_or_else(|| panic!("no action {name}"))
                .parameters
                .to_string()
        };
        assert!(
            !schema_of("arrange_room").contains("\"spot\""),
            "the agent's schema must not mention a rectangle it may not send"
        );
        assert!(
            schema_of("room_arrange").contains("\"spot\""),
            "the person's own action is where a dragged rectangle arrives"
        );

        // The same rectangle through the person's own action is accepted.
        call(
            &extension,
            &state,
            "room_arrange",
            json!({
                "expected_revision": revision(&state),
                "panes": [{ "id": "start", "spot": { "x": 10.0, "y": 10.0, "w": 400.0, "h": 300.0 } }]
            }),
        )
        .expect("the person's drag is honoured");
    }

    #[test]
    fn the_rooms_own_read_back_describes_position_without_a_single_number() {
        // layout::describe has its own version of this check; this one is the
        // guard on the seam — that the section actually reaches read_room, and
        // that nothing on the way in re-introduces a coordinate.
        let (state, extension) = fresh("no-numbers");
        call(
            &extension,
            &state,
            "room_arrange",
            json!({
                "expected_revision": revision(&state),
                "panes": [{ "id": "start", "spot": { "x": 1234.0, "y": 567.0, "w": 480.0, "h": 321.0 } }]
            }),
        )
        .expect("the person drags a pane to an awkward spot");

        let read = state.read().expect("read back");
        let section = read
            .split("WHERE THINGS SIT")
            .nth(1)
            .expect("the read-back describes where things sit")
            .split("\nTo move a pane")
            .next()
            .expect("section ends");
        assert!(
            !section.chars().any(|c| c.is_ascii_digit()),
            "the position section leaked a number:\n{section}"
        );
        assert!(
            !read.contains("1234") && !read.contains("567"),
            "the rectangle the person dragged to must not appear anywhere:\n{read}"
        );
    }

    #[test]
    fn a_human_drag_reads_back_as_a_relation_not_a_position() {
        let (state, extension) = fresh("drag-delta");
        let options = spot_of(&state, "options");
        call(
            &extension,
            &state,
            "room_arrange",
            json!({
                "expected_revision": revision(&state),
                "panes": [{
                    "id": "start",
                    "spot": { "x": options.right() + 20.0, "y": options.y, "w": 400.0, "h": 300.0 }
                }]
            }),
        )
        .expect("the person drags a pane beside another");

        let read = state.read().expect("read back");
        assert!(
            read.contains("moved “Where do you want to take this?”"),
            "the delta should name the pane that moved:\n{read}"
        );
        assert!(
            read.contains("right of “What this workspace can run”"),
            "the delta should say where it ended up, relationally:\n{read}"
        );
    }

    #[test]
    fn a_view_the_renderer_could_not_trust_never_reaches_state() {
        let (state, extension) = fresh("hostile");
        for hostile in [
            json!({ "kind": "embed", "url": "javascript:alert(1)" }),
            json!({ "kind": "image", "src": "https://evil.example/pixel.png" }),
            json!({ "kind": "script", "text": "alert(1)" }),
        ] {
            let error = call(
                &extension,
                &state,
                "put_pane",
                json!({ "expected_revision": revision(&state), "id": "x", "title": "x", "view": hostile }),
            )
            .expect_err("hostile view is refused");
            assert!(!error.is_empty());
        }
        assert!(!state.doc.lock().panes.iter().any(|pane| pane.id == "x"));
    }

    #[test]
    fn hostile_text_survives_as_inert_text() {
        let (state, extension) = fresh("inert");
        let hostile = "<img src=x onerror=\"globalThis.pwned=true\"><script>alert(1)</script>";
        call(
            &extension,
            &state,
            "put_pane",
            json!({ "expected_revision": revision(&state), "id": "inert", "title": "Inert", "view": text_view(hostile) }),
        )
        .expect("markup is ordinary text here");
        let resolved = state.resolved();
        let stored = resolved["panes"]
            .as_array()
            .expect("panes")
            .iter()
            .find(|pane| pane["id"] == "inert")
            .expect("pane");
        assert_eq!(stored["view"]["children"][0]["text"], hostile);
    }

    #[test]
    fn the_theme_only_accepts_its_own_tokens() {
        let (state, extension) = fresh("theme");
        for bad in [
            json!({ "accent": "red; background: url(x)" }),
            json!({ "surface": "neon" }),
            json!({ "scale": 4 }),
        ] {
            call(
                &extension,
                &state,
                "configure_room",
                json!({ "expected_revision": revision(&state), "theme": bad }),
            )
            .expect_err("an out-of-vocabulary theme token is refused");
        }
        call(
            &extension,
            &state,
            "configure_room",
            json!({ "expected_revision": revision(&state), "theme": { "surface": "paper", "accent": "#AA3311" } }),
        )
        .expect("a valid theme lands");
        let doc = state.doc.lock();
        assert_eq!(doc.theme.surface, "paper");
        assert_eq!(doc.theme.accent, "#aa3311");
    }

    #[test]
    fn a_source_pane_resolves_against_the_live_file() {
        let (state, extension) = fresh("source");
        call(
            &extension,
            &state,
            "put_pane",
            json!({
                "expected_revision": revision(&state),
                "id": "src",
                "title": "Workspace manifest",
                "view": { "kind": "source", "path": "Cargo.toml", "from": 1, "to": 2 }
            }),
        )
        .expect("source pane lands");
        let resolved = state.resolved();
        let pane = resolved["panes"]
            .as_array()
            .expect("panes")
            .iter()
            .find(|pane| pane["id"] == "src")
            .expect("pane");
        assert!(
            pane["view"]["resolved"]["text"]
                .as_str()
                .expect("resolved text")
                .contains("[workspace]"),
            "{pane:?}"
        );
    }

    #[test]
    fn a_source_pane_that_stopped_resolving_says_so_instead_of_vanishing() {
        let (state, extension) = fresh("source-error");
        call(
            &extension,
            &state,
            "put_pane",
            json!({
                "expected_revision": revision(&state),
                "id": "gone",
                "title": "Missing",
                "view": { "kind": "source", "path": "does/not/exist.rs" }
            }),
        )
        .expect("the path is well formed, so the pane is accepted");
        let resolved = state.resolved();
        let pane = resolved["panes"]
            .as_array()
            .expect("panes")
            .iter()
            .find(|pane| pane["id"] == "gone")
            .expect("pane");
        assert!(pane["view"]["error"].is_string(), "{pane:?}");
    }

    #[test]
    fn the_document_survives_a_reopen() {
        let path = std::env::temp_dir().join(format!(
            "same-page-room-test-{}-reopen.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("workspace root");
        let workspace = Arc::new(Workspace::open(root).expect("workspace opens"));
        let state =
            RoomState::open(test_transport(), path.clone(), workspace.clone()).expect("open");
        let extension = RoomExtension::new(state.clone());
        call(
            &extension,
            &state,
            "room_put_pane",
            json!({ "expected_revision": revision(&state), "id": "mine", "title": "Mine", "view": text_view("kept") }),
        )
        .expect("the person writes a pane");
        drop(state);

        let reopened = RoomState::open(test_transport(), path, workspace).expect("reopen");
        let doc = reopened.doc.lock();
        let pane = doc
            .panes
            .iter()
            .find(|pane| pane.id == "mine")
            .expect("pane");
        assert_eq!(pane.author, Author::You, "authorship survives the restart");
    }
}
