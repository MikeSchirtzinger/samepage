//! The SamePage shared document.
//!
//! One Yrs document per peer, in one layout, owned by this crate:
//!
//! - [`Document`]: the replica itself, the only code that opens a Yrs
//!   transaction. Browser and host both use it.
//! - [`Replica`]: the browser side. It plans through C1 capabilities, applies
//!   resolved ops locally and queues them for the host. It never writes
//!   authorship.
//! - [`host::Host`] (feature `host`): the authority. It resolves every batch
//!   itself, admits browser frames only in a settled, audited state, stamps
//!   the authenticated actor in the same transaction as the change, keeps the
//!   commit journal and persists the document.
//!
//! Capabilities depend on `samepage-capability`, not on this crate; they see
//! `View`s and return plans.

pub mod document;
#[cfg(feature = "host")]
pub mod host;
#[cfg(feature = "host")]
pub mod journal;
pub mod layout;
pub mod replica;
pub mod sync;

use serde::{Deserialize, Serialize};

pub use document::{DocError, Document, IdMinter};
pub use replica::{Replica, ReplicaError};

/// ABI string of the browser document artifact.
pub const DOCUMENT_ABI: &str = "samepage.document.v1";
/// Layout version: one `objects` root of flat scalar records.
pub const LAYOUT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Caller {
    Human,
    Agent,
}

impl Caller {
    pub fn as_str(self) -> &'static str {
        match self {
            Caller::Human => "human",
            Caller::Agent => "agent",
        }
    }
}

/// Who made a change. Built by the host transport from an authenticated
/// connection or dispatcher caller; never read from a frame, plan or batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    /// Host-minted participant id, stable across that participant's connections.
    pub participant: String,
    pub caller: Caller,
    /// Display name, if the participant announced one.
    #[serde(default)]
    pub label: Option<String>,
}

impl Actor {
    pub fn new(participant: impl Into<String>, caller: Caller) -> Self {
        Self {
            participant: participant.into(),
            caller,
            label: None,
        }
    }

    pub fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The name written to `created_by` / `touched_by`.
    pub fn byline(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| self.caller.as_str().to_string())
    }
}

impl From<&Actor> for samepage_capability::Principal {
    fn from(actor: &Actor) -> Self {
        samepage_capability::Principal {
            participant: actor.participant.clone(),
            caller: match actor.caller {
                Caller::Human => samepage_capability::CallerKind::Human,
                Caller::Agent => samepage_capability::CallerKind::Agent,
            },
            label: actor.label.clone(),
        }
    }
}
