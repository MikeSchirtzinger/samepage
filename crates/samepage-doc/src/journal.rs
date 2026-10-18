//! The commit journal: who changed the document, in what order.
//!
//! This is a document primitive, not telemetry. Every accepted change gets a
//! sequence number and the authenticated actor that made it, and read-back
//! ("changed since you last read", bylines) is built from it and from the
//! host-written authorship keys. Activity timelines, spans and their UI are a
//! separate optional capability; nothing here depends on them and nothing
//! there is consulted for attribution.
//!
//! The journal also holds client-id registration: which authenticated
//! participant introduced each Yrs client id, and which ids are reserved (the
//! host's own, and every id already in a document when it was opened).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::Actor;

/// How many entries the journal keeps in memory and on disk.
pub const RECENT: usize = 512;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub actor: Actor,
    /// The preferred action name, or `None` for a replica frame.
    pub action: Option<String>,
    pub revision: String,
    pub created: Vec<String>,
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
    pub visible_change: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    pub layout_version: u32,
    pub seq: u64,
    /// Participant -> latest sequence number they committed.
    pub latest: BTreeMap<String, u64>,
    pub recent: VecDeque<Entry>,
    /// Yrs client id -> participant that registered it.
    pub bindings: BTreeMap<u64, String>,
    /// Client ids nobody may register: host ids and ids already present when
    /// a document was opened without a record of who made them.
    pub reserved: BTreeSet<u64>,
}

impl Journal {
    pub fn record(&mut self, mut entry: Entry) -> u64 {
        self.seq += 1;
        entry.seq = self.seq;
        self.latest
            .insert(entry.actor.participant.clone(), self.seq);
        self.recent.push_back(entry);
        while self.recent.len() > RECENT {
            self.recent.pop_front();
        }
        self.seq
    }

    /// Who may introduce new content under `client`.
    pub fn may_use(&self, client: u64, participant: &str) -> bool {
        if self.reserved.contains(&client) {
            return false;
        }
        match self.bindings.get(&client) {
            Some(owner) => owner == participant,
            None => true,
        }
    }
}
