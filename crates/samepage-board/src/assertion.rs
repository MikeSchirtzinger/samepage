//! The assertion vocabulary.
//!
//! An assertion says what something *means* — never where it sits. There are no
//! coordinates in this file and there is no node that describes appearance. A
//! claim is a claim whether it renders as a card, a row, or a line of text, and
//! the same assertion stream can drive a very different picture without the
//! writer knowing or caring.
//!
//! That is the whole reason the vocabulary is semantic rather than visual. Two
//! things depend on it:
//!
//! - **Read-back is honest by construction.** The host still holds the meaning
//!   at the moment it renders, so describing the surface back to an agent is a
//!   projection, not an inference. Nothing has to reconstruct intent from
//!   geometry after the fact.
//! - **One vocabulary, two runtimes.** The agent writes assertions directly in
//!   the fast lane. A sandboxed component emits the same assertions across the
//!   WASI boundary. Neither producer gets its own dialect.
//!
//! Every assertion carries an `id`, which is what relations point at and what
//! lets a human mark survive the agent rewriting the thing underneath it.
//!
//! This file is the vocabulary only. What carries it — actions, audiences,
//! persistence, the live event — is the native board adapter, which composes into the
//! same host as the map so both halves of a conversation share one process,
//! one action catalog, and one event stream.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

/// Who wrote something. Not an honour-system field: the host sets it from which
/// endpoint the write arrived through, and the two endpoints have different
/// audiences.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    Agent,
    Named(String),
    You,
}

impl Author {
    pub fn word(&self) -> &str {
        match self {
            Author::Agent => "agent",
            Author::Named(label) => label,
            Author::You => "you",
        }
    }
}

/// How settled a claim is. The agent may propose, support and contest; only a
/// human mark can make something `Settled` — see [`Mark::Agree`].
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Proposed,
    Supported,
    Contested,
    Settled,
}

/// What one assertion has to do with another. This is the load-bearing half of
/// the vocabulary — a brainstorm is mostly relations, and a picture that shows
/// boxes without them is a list with extra steps.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Supports,
    Contradicts,
    DependsOn,
    Refines,
    Answers,
}

impl RelationKind {
    pub fn word(self) -> &'static str {
        match self {
            RelationKind::Supports => "supports",
            RelationKind::Contradicts => "contradicts",
            RelationKind::DependsOn => "depends on",
            RelationKind::Refines => "refines",
            RelationKind::Answers => "answers",
        }
    }
}

/// Whether a piece of evidence actually held up. `Unverified` is the default on
/// purpose: an agent citing something is not the same as having checked it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    #[default]
    Unverified,
    Confirmed,
    Refuted,
}

/// A human-only annotation. There is deliberately no agent-audience action that
/// writes one, so a `✓` on the board is always something a person put there and
/// the agent cannot mark its own work as agreed.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mark {
    Question,
    Important,
    Agree,
    Disagree,
}

impl Mark {
    pub fn glyph(self) -> &'static str {
        match self {
            Mark::Question => "?",
            Mark::Important => "!",
            Mark::Agree => "✓",
            Mark::Disagree => "✗",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Mark::Question => "question",
            Mark::Important => "important",
            Mark::Agree => "agree",
            Mark::Disagree => "disagree",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "question" => Ok(Mark::Question),
            "important" => Ok(Mark::Important),
            "agree" => Ok(Mark::Agree),
            "disagree" => Ok(Mark::Disagree),
            other => Err(format!(
                "unknown mark `{other}` (question, important, agree, disagree)"
            )),
        }
    }
}

/// One thing said about the subject under discussion.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Assertion {
    /// Something held to be the case.
    Claim {
        id: String,
        text: String,
        #[serde(default)]
        status: Status,
    },
    /// Something not yet known. `blocking` means work stops until it is
    /// answered, which is what separates a real open question from a musing.
    Question {
        id: String,
        text: String,
        #[serde(default)]
        blocking: bool,
    },
    /// One candidate among several. `tradeoff` is what it costs — an option
    /// listed without one is usually a preference in disguise.
    Choice {
        id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tradeoff: Option<String>,
    },
    /// A directed link between two other assertions, by id.
    ///
    /// The field is `how` rather than `kind` because `kind` is the enum's own
    /// serde tag — `{"kind":"relation","from":"a","to":"b","how":"supports"}`.
    Relation {
        id: String,
        from: String,
        to: String,
        how: RelationKind,
    },
    /// A named set of other assertions, by id. Membership is meaning, not
    /// layout — the renderer may or may not draw a box around them.
    Group {
        id: String,
        label: String,
        #[serde(default)]
        members: Vec<String>,
    },
    /// Something checked, and how it came out.
    Evidence {
        id: String,
        about: String,
        source: String,
        #[serde(default)]
        verdict: Verdict,
    },
    /// A fork closed. `over` names what was rejected, `because` says why —
    /// both required, because a decision without them is unreviewable later.
    Decision {
        id: String,
        chose: String,
        #[serde(default)]
        over: Vec<String>,
        because: String,
    },
}

impl Assertion {
    pub fn id(&self) -> &str {
        match self {
            Assertion::Claim { id, .. }
            | Assertion::Question { id, .. }
            | Assertion::Choice { id, .. }
            | Assertion::Relation { id, .. }
            | Assertion::Group { id, .. }
            | Assertion::Evidence { id, .. }
            | Assertion::Decision { id, .. } => id,
        }
    }

    /// The word used for this assertion in read-back and in the catalog.
    pub fn kind_word(&self) -> &'static str {
        match self {
            Assertion::Claim { .. } => "claim",
            Assertion::Question { .. } => "question",
            Assertion::Choice { .. } => "choice",
            Assertion::Relation { .. } => "relation",
            Assertion::Group { .. } => "group",
            Assertion::Evidence { .. } => "evidence",
            Assertion::Decision { .. } => "decision",
        }
    }

    /// Ids this assertion points at. Used to reject dangling references at
    /// write time rather than rendering a relation to nothing.
    pub fn references(&self) -> Vec<&str> {
        match self {
            Assertion::Relation { from, to, .. } => vec![from.as_str(), to.as_str()],
            Assertion::Group { members, .. } => members.iter().map(String::as_str).collect(),
            Assertion::Evidence { about, .. } => vec![about.as_str()],
            Assertion::Decision { chose, over, .. } => std::iter::once(chose.as_str())
                .chain(over.iter().map(String::as_str))
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// One assertion as the board holds it: the assertion itself, who wrote it, the
/// revision it last changed at, and any human mark.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Entry {
    pub assertion: Assertion,
    pub author: Author,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_participant: Option<String>,
    /// UTC timestamp for the current assertion text. Additive so boards
    /// written before cementing still load; an old entry without one cannot
    /// be cemented because the receipt would otherwise have to invent it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authored_at: Option<String>,
    /// Board revision at which the current assertion text was authored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authored_at_revision: Option<u64>,
    pub changed_at: u64,
    pub mark: Option<(Mark, Option<String>)>,
    /// Host-stamped signer for the current mark. The only writer is
    /// `Board::mark_at`, reached through the existing human-only action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marked_by: Option<Author>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marked_by_participant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marked_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marked_at_revision: Option<u64>,
}

/// Everything currently asserted, plus enough history to answer "what changed
/// since you last looked".
///
/// `revision` counts writes. `agent_saw` and `human_saw` are **session** state:
/// they record where each side's attention was, not anything about the content.
/// A delta computed against them is therefore "new to you", which is the only
/// kind of delta worth showing a person.
/// `entries`, `revision` and `subject` are the document and survive a restart.
/// `agent_saw` and `human_saw` deliberately do not: a restart is a new session,
/// and a new session has not looked at anything yet. Persisting them would make
/// "new to you" a property of the file rather than of the reader.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Board {
    entries: Vec<Entry>,
    /// Display names supplied by the authenticated native host. These are
    /// presentation data, never roles or authorization input. Retaining them
    /// lets a saved sign-off name its signer after presence expires or restart.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    participants: BTreeMap<String, String>,
    revision: u64,
    #[serde(skip)]
    agent_saw: u64,
    #[serde(skip)]
    human_saw: u64,
    subject: Option<String>,
}

/// What changed for one audience since it last looked.
///
/// There is no `added` beside `changed`: an assertion is identified by its id
/// and writing a known id replaces it, so "new" and "rewritten" are the same
/// event to a reader who has to go look at it either way.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Delta {
    /// The revision this audience had last seen. Shown to the human so the
    /// board can say what "new" is being measured from.
    pub since: u64,
    pub changed: Vec<String>,
    pub marked: Vec<String>,
}

impl Delta {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.marked.is_empty()
    }
}

impl Board {
    pub fn bind_participant(&mut self, participant: &str, name: &str) {
        if !participant.trim().is_empty() && !name.trim().is_empty() {
            self.participants.insert(participant.into(), name.into());
        }
    }

    /// Resolve only the participant stored on this mark. `You` remains a role
    /// in the persisted entry; it is not a viewer-independent display name.
    pub fn marker_byline<'a>(&'a self, entry: &Entry, reader: Option<&str>) -> Option<&'a str> {
        entry.mark.as_ref()?;
        let participant = entry
            .marked_by_participant
            .as_deref()
            .filter(|p| !p.trim().is_empty())?;
        if reader == Some(participant) {
            Some("you")
        } else {
            self.participants
                .get(participant)
                .map(String::as_str)
                .filter(|name| !name.trim().is_empty())
        }
    }

    pub fn marked_changes(&self, ids: &[String], reader: Option<&str>) -> Vec<String> {
        let mut markers: BTreeMap<Option<&str>, Vec<&str>> = BTreeMap::new();
        for id in ids {
            if let Some(entry) = self.get(id).filter(|entry| entry.mark.is_some()) {
                markers
                    .entry(self.marker_byline(entry, reader))
                    .or_default()
                    .push(id);
            }
        }
        markers
            .into_iter()
            .map(|(marker, ids)| match marker {
                Some(marker) => format!("marked by {marker}: {}", ids.join(", ")),
                None => format!("marked: {} (marker identity unavailable)", ids.join(", ")),
            })
            .collect()
    }

    pub fn bind_author(&mut self, id: &str, participant: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.assertion.id() == id) {
            entry.author_participant = Some(participant.into());
        }
    }
    pub fn bind_signer(&mut self, id: &str, participant: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.assertion.id() == id) {
            entry.marked_by_participant = Some(participant.into());
        }
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn subject(&self) -> Option<&str> {
        self.subject.as_deref()
    }

    pub fn set_subject(&mut self, subject: String) {
        self.subject = Some(subject);
        self.revision += 1;
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.assertion.id() == id)
    }

    /// Write an assertion, creating or replacing by id.
    ///
    /// A rewrite keeps the existing mark. That is the point: if a person marks
    /// something `?` and the agent rewrites it to answer them, the question was
    /// still theirs to close.
    pub fn assert_at(
        &mut self,
        assertion: Assertion,
        author: Author,
        authored_at: String,
    ) -> Result<String, String> {
        if authored_at.trim().is_empty() {
            return Err("host timestamp is required".into());
        }
        if matches!(
            &assertion,
            Assertion::Claim {
                status: Status::Settled,
                ..
            }
        ) {
            return Err(format!(
                "`{}`: only a human agree mark settles a claim",
                assertion.id()
            ));
        }
        validate(&assertion)?;
        for reference in assertion.references() {
            if reference == assertion.id() {
                return Err(format!("`{}` refers to itself", assertion.id()));
            }
            if self.get(reference).is_none() {
                return Err(format!(
                    "`{}` refers to `{reference}`, which is not on the board",
                    assertion.id()
                ));
            }
        }

        self.revision += 1;
        let id = assertion.id().to_string();
        let verb = match self
            .entries
            .iter_mut()
            .find(|entry| entry.assertion.id() == id)
        {
            Some(existing) => {
                existing.assertion = assertion;
                existing.author = author;
                existing.authored_at = Some(authored_at);
                existing.authored_at_revision = Some(self.revision);
                existing.changed_at = self.revision;
                "rewrote"
            }
            None => {
                self.entries.push(Entry {
                    assertion,
                    author,
                    author_participant: None,
                    authored_at: Some(authored_at),
                    authored_at_revision: Some(self.revision),
                    changed_at: self.revision,
                    mark: None,
                    marked_by: None,
                    marked_by_participant: None,
                    marked_at: None,
                    marked_at_revision: None,
                });
                "added"
            }
        };
        Ok(format!("{verb} {id}"))
    }

    /// Remove an assertion, and anything left pointing at it.
    pub fn retract(&mut self, id: &str) -> Result<String, String> {
        if self.get(id).is_none() {
            return Err(format!("`{id}` is not on the board"));
        }
        let mut gone = std::collections::BTreeSet::from([id.to_string()]);
        loop {
            let before = gone.len();
            for entry in &self.entries {
                if entry
                    .assertion
                    .references()
                    .iter()
                    .any(|r| gone.contains(*r))
                {
                    gone.insert(entry.assertion.id().to_string());
                }
            }
            if gone.len() == before {
                break;
            }
        }
        let orphaned: Vec<String> = gone.iter().filter(|r| r.as_str() != id).cloned().collect();
        self.entries
            .retain(|entry| !gone.contains(entry.assertion.id()));
        self.revision += 1;
        Ok(if orphaned.is_empty() {
            format!("retracted {id}")
        } else {
            format!("retracted {id}, and {} left dangling", orphaned.join(", "))
        })
    }

    /// Human-only. There is no agent-audience path to this.
    pub fn mark_at(
        &mut self,
        id: &str,
        mark: Mark,
        note: Option<String>,
        marked_at: String,
    ) -> Result<String, String> {
        if marked_at.trim().is_empty() {
            return Err("host timestamp is required".into());
        }
        if note.as_ref().is_some_and(|s| s.len() > MAX_TEXT) {
            return Err("mark note exceeds 4000 characters".into());
        }
        let revision = self.revision + 1;
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.assertion.id() == id)
            .ok_or_else(|| format!("`{id}` is not on the board"))?;
        entry.mark = Some((mark, note));
        entry.marked_by = Some(Author::You);
        // A new mark cannot inherit the previous person's identity. The host
        // transition binds the actual signer after this succeeds.
        entry.marked_by_participant = None;
        entry.marked_at = Some(marked_at);
        entry.marked_at_revision = Some(revision);
        entry.changed_at = revision;
        // A human agreeing is the only thing that settles a claim.
        if mark == Mark::Agree
            && let Assertion::Claim { status, .. } = &mut entry.assertion {
                *status = Status::Settled;
            }
        if mark != Mark::Agree
            && let Assertion::Claim { status, .. } = &mut entry.assertion
                && *status == Status::Settled {
                    *status = Status::Proposed;
                }
        self.revision = revision;
        Ok(format!("marked {id} {}", mark.word()))
    }

    /// Human-only, and the exact inverse of [`mark_at`](Self::mark_at): withdrawing
    /// an `Agree` also withdraws the settlement it caused. Otherwise the board
    /// would carry a claim marked `settled` with nobody's agreement behind it,
    /// which is precisely the unearned authority the human-only marks exist to
    /// prevent.
    ///
    /// It drops to `Proposed` rather than back to whatever the agent had
    /// written, because the pre-agreement status is not kept. Understating what
    /// a claim has earned is the safe direction to be wrong in.
    pub fn unmark(&mut self, id: &str) -> Result<String, String> {
        let revision = self.revision + 1;
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.assertion.id() == id)
            .ok_or_else(|| format!("`{id}` is not on the board"))?;
        let withdrawn = entry.mark.take().map(|(mark, _)| mark);
        entry.marked_by = None;
        entry.marked_by_participant = None;
        entry.marked_at = None;
        entry.marked_at_revision = None;
        if withdrawn == Some(Mark::Agree)
            && let Assertion::Claim { status, .. } = &mut entry.assertion
                && *status == Status::Settled {
                    *status = Status::Proposed;
                }
        entry.changed_at = revision;
        self.revision = revision;
        Ok(format!("unmarked {id}"))
    }

    pub fn delta_since(&self, since: u64) -> Delta {
        let mut delta = Delta {
            since,
            changed: Vec::new(),
            marked: Vec::new(),
        };
        for entry in &self.entries {
            if entry.changed_at <= since {
                continue;
            }
            let id = entry.assertion.id().to_string();
            if entry.mark.is_some() {
                delta.marked.push(id);
            } else {
                delta.changed.push(id);
            }
        }
        delta
    }

    pub fn human_delta(&self) -> Delta {
        self.delta_since(self.human_saw)
    }

    pub fn acknowledge_human(&mut self) {
        self.human_saw = self.revision;
    }

    /// The agent's read-back: what is on the board, what it means, and what
    /// changed since the agent last read. Relations and marks — no geometry.
    ///
    /// Reading *moves* the agent's delta, which is why this takes `&mut self`
    /// and why the composite host's own `describe()` calls [`peek`](Self::peek)
    /// instead. A description rendered for some other purpose must not consume
    /// the agent's "since you last looked" — that is a fact about the reader.
    pub fn read(&mut self) -> String {
        let text = self.render(self.agent_saw);
        self.agent_saw = self.revision;
        text
    }

    /// The same read-back, without moving the agent's delta.
    pub fn peek(&self) -> String {
        self.render(self.agent_saw)
    }

    pub fn render(&self, since: u64) -> String {
        self.render_for(since, None)
    }

    pub fn render_for(&self, since: u64, reader: Option<&str>) -> String {
        let delta = self.delta_since(since);

        let mut out = String::new();
        out.push_str(&format!("revision {}\n", self.revision));
        if let Some(subject) = &self.subject {
            out.push_str(&format!("subject: {subject}\n"));
        }
        if self.entries.is_empty() {
            out.push_str("\nThe board is empty.\n");
        }

        let mut relations: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for entry in &self.entries {
            if let Assertion::Relation { from, to, how, .. } = &entry.assertion {
                relations
                    .entry(from.as_str())
                    .or_default()
                    .push(format!("{} {to}", how.word()));
            }
        }

        for entry in &self.entries {
            if matches!(entry.assertion, Assertion::Relation { .. }) {
                continue;
            }
            out.push('\n');
            out.push_str(&describe(&entry.assertion));
            out.push_str(&format!(
                " [{}, by {}]",
                entry.assertion.id(),
                entry.author.word()
            ));
            if let Some((mark, note)) = &entry.mark {
                out.push_str(&format!("\n  marked {}", mark.word()));
                match self.marker_byline(entry, reader) {
                    Some(marker) => out.push_str(&format!(" by {marker}")),
                    None => out.push_str(" (marker identity unavailable)"),
                }
                if let Some(note) = note {
                    out.push_str(&format!(": {note}"));
                }
            }
            if entry.mark.as_ref().is_some_and(|(m, _)| *m == Mark::Agree) {
                out.push_str(if fresh_agreement(entry) {
                    "\n  SETTLED: fresh human sign-off"
                } else {
                    "\n  STALE: human sign-off does not cover the current words"
                });
            }
            if let Some(links) = relations.get(entry.assertion.id()) {
                for link in links {
                    out.push_str(&format!("\n  {link}"));
                }
            }
            out.push('\n');
        }

        out.push_str("\n--- since you last read ---\n");
        if delta.is_empty() {
            out.push_str("nothing changed.\n");
        } else {
            if !delta.changed.is_empty() {
                out.push_str(&format!("written: {}\n", delta.changed.join(", ")));
            }
            if !delta.marked.is_empty() {
                for change in self.marked_changes(&delta.marked, reader) {
                    out.push_str(&format!("{change}\n"));
                }
            }
        }
        out.push_str("\nThe vocabulary is at GET /board/primitives.\n");
        out
    }

    /// Wipe the board back to nothing. Human-only, like the marks: an agent
    /// that could erase the record of what was agreed could erase a
    /// disagreement it lost.
    pub fn clear(&mut self) -> String {
        let count = self.entries.len();
        self.entries.clear();
        self.subject = None;
        self.revision += 1;
        // Not reset to zero: a revision that goes backwards would make every
        // surviving reader's "since I last looked" silently wrong.
        self.agent_saw = self.revision;
        self.human_saw = self.revision;
        format!("cleared the board ({count} gone)")
    }
}

/// Render one assertion as a sentence. Used by read-back and by deixis, and
/// deliberately not by the browser's renderer — the two are allowed to differ,
/// because one is for a model and the other is for eyes.
pub fn describe(assertion: &Assertion) -> String {
    match assertion {
        Assertion::Claim { text, status, .. } => {
            format!("claim ({status}): {text}", status = status_word(*status))
        }
        Assertion::Question { text, blocking, .. } => {
            let blocking = if *blocking { ", blocking" } else { "" };
            format!("question{blocking}: {text}")
        }
        Assertion::Choice { text, tradeoff, .. } => match tradeoff {
            Some(tradeoff) => format!("choice: {text} — costs: {tradeoff}"),
            None => format!("choice: {text} (no tradeoff stated)"),
        },
        Assertion::Relation { from, to, how, .. } => {
            format!("relation: {from} {} {to}", how.word())
        }
        Assertion::Group { label, members, .. } => {
            format!("group \"{label}\": {}", members.join(", "))
        }
        Assertion::Evidence {
            about,
            source,
            verdict,
            ..
        } => format!(
            "evidence on {about} ({}): {source}",
            match verdict {
                Verdict::Confirmed => "confirmed",
                Verdict::Refuted => "refuted",
                Verdict::Unverified => "unverified",
            }
        ),
        Assertion::Decision {
            chose,
            over,
            because,
            ..
        } => {
            let over = if over.is_empty() {
                String::new()
            } else {
                format!(" over {}", over.join(", "))
            };
            format!("decision: chose {chose}{over} because {because}")
        }
    }
}

pub fn status_word(status: Status) -> &'static str {
    match status {
        Status::Proposed => "proposed",
        Status::Supported => "supported",
        Status::Contested => "contested",
        Status::Settled => "settled",
    }
}

const MAX_TEXT: usize = 4_000;
const MAX_ID: usize = 80;

pub fn validate(assertion: &Assertion) -> Result<(), String> {
    let id = assertion.id();
    if id.is_empty() || id.len() > MAX_ID {
        return Err(format!("id must be 1..={MAX_ID} characters"));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("id `{id}` must be ascii alphanumeric, `-` or `_`"));
    }
    let text = match assertion {
        Assertion::Claim { text, .. }
        | Assertion::Question { text, .. }
        | Assertion::Choice { text, .. } => Some(text),
        Assertion::Group { label, .. } => Some(label),
        Assertion::Decision { because, .. } => Some(because),
        Assertion::Evidence { source, .. } => Some(source),
        Assertion::Relation { .. } => None,
    };
    if let Some(text) = text {
        if text.trim().is_empty() {
            return Err(format!("`{id}` has no text"));
        }
        if text.len() > MAX_TEXT {
            return Err(format!("`{id}` exceeds {MAX_TEXT} characters"));
        }
    }
    if let Assertion::Decision { chose, because, .. } = assertion
        && (chose.trim().is_empty() || because.trim().is_empty()) {
            return Err(format!("`{id}` needs both `chose` and `because`"));
        }
    Ok(())
}

/// The vocabulary, described for whoever is writing it — agent or human.
///
/// This is generated rather than written prose so it cannot drift from the
/// enum. `catalog_covers_every_variant` in the tests below fails if a variant
/// is added without describing it here.
pub fn catalog() -> JsonValue {
    json!({
        "note": "Assertions carry meaning, never position. The host decides how each one looks.",
        "assertions": [
            {
                "kind": "claim",
                "says": "something held to be the case",
                "fields": {"id": "string", "text": "string", "status": "proposed | supported | contested | settled"},
                "notes": "only a human `agree` mark can move a claim to settled",
                "example": {"kind": "claim", "id": "wasm-proven", "text": "The component sandbox passes 12/12 gates.", "status": "supported"}
            },
            {
                "kind": "question",
                "says": "something not yet known",
                "fields": {"id": "string", "text": "string", "blocking": "bool"},
                "notes": "`blocking` means work stops until it is answered",
                "example": {"kind": "question", "id": "event-shape", "text": "What does a component emit so a person sees something?", "blocking": true}
            },
            {
                "kind": "choice",
                "says": "one candidate among several",
                "fields": {"id": "string", "text": "string", "tradeoff": "string?"},
                "notes": "a choice with no tradeoff is usually a preference in disguise",
                "example": {"kind": "choice", "id": "fast-lane", "text": "axum + htmx, no build step", "tradeoff": "no sandbox; trusted input only"}
            },
            {
                "kind": "relation",
                "says": "a directed link between two assertions",
                "fields": {"id": "string", "from": "id", "to": "id", "how": "supports | contradicts | depends_on | refines | answers"},
                "notes": "both endpoints must already exist; a dangling relation is refused. The field is `how`, not `kind` — `kind` is the tag that selects the assertion itself",
                "example": {"kind": "relation", "id": "r1", "from": "wasm-proven", "to": "sandboxed-lane", "how": "supports"}
            },
            {
                "kind": "group",
                "says": "a named set of assertions",
                "fields": {"id": "string", "label": "string", "members": "[id]"},
                "notes": "membership is meaning, not layout",
                "example": {"kind": "group", "id": "tiers", "label": "Two runtimes", "members": ["fast-lane", "sandboxed-lane"]}
            },
            {
                "kind": "evidence",
                "says": "something checked, and how it came out",
                "fields": {"id": "string", "about": "id", "source": "string", "verdict": "unverified | confirmed | refuted"},
                "notes": "defaults to unverified — citing is not checking",
                "example": {"kind": "evidence", "id": "e1", "about": "wasm-proven", "source": "cargo run -p component-host-probe → exit 0", "verdict": "confirmed"}
            },
            {
                "kind": "decision",
                "says": "a fork closed",
                "fields": {"id": "string", "chose": "id", "over": "[id]", "because": "string"},
                "notes": "`because` is required — a decision without it is unreviewable later",
                "example": {"kind": "decision", "id": "d1", "chose": "fast-lane", "over": ["sandboxed-lane"], "because": "no ceremony for the yolo tier"}
            }
        ],
        "marks": {
            "note": "human-only. The agent has no action that writes one.",
            "values": ["question", "important", "agree", "disagree"]
        },
        "actions": {
            "note": "audience is enforced by the host at dispatch, not by a field in the payload. An agent calling a human action is refused before its arguments are even read.",
            "agent": {
                "board_read": "the read-back, including what changed since this agent last read",
                "board_assert": "write one assertion, creating or replacing by id",
                "board_retract": "remove one, and anything left pointing at it",
                "board_subject": "name what the board is about",
                "board_cement_preview": "preview the exact advisory Govern draft and receipt; writes nothing"
            },
            "human": {
                "board_compose": "write a claim or a question in your own name",
                "board_mark": "?, !, ✓ or ✗ on any assertion — the agent cannot do this",
                "board_unmark": "take a mark back",
                "board_seen": "say you have looked, which is what moves your delta",
                "board_clear": "wipe the board and start over",
                "board_cement": "write the settled board and Atlas revision into a new directory"
            }
        }
    })
}

/// A mark is settlement only when it covers these exact words and has provenance.
pub fn fresh_agreement(entry: &Entry) -> bool {
    entry.mark.as_ref().is_some_and(|(m, _)| *m == Mark::Agree)
        && entry.marked_by == Some(Author::You)
        && entry.authored_at.as_ref().is_some_and(|s| !s.is_empty())
        && entry.marked_at.as_ref().is_some_and(|s| !s.is_empty())
        && entry.marked_at_revision == Some(entry.changed_at)
        && matches!((entry.authored_at_revision, entry.marked_at_revision), (Some(a), Some(m)) if m >= a)
        && !matches!(entry.assertion, Assertion::Claim { status, .. } if status != Status::Settled)
}
