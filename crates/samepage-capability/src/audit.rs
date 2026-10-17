//! The host's check on a browser replica's frame.
//!
//! A replica frame is a CRDT update, not a call, so it can carry any key with
//! any value. The host applies it to a disposable candidate, builds a [`View`]
//! of the document before and after, and asks [`audit`] whether what changed
//! is allowed. The audit looks only at the [`Delta`]:
//!
//! - authorship keys and kernel-reserved keys may not be written by a replica
//!   at all; the host stamps authorship itself once the frame is accepted;
//! - every created, changed or removed key must belong to some capability in
//!   the running composition;
//! - records of kinds the composition does not own may not be created,
//!   changed, re-kinded or deleted.
//!
//! What the frame did not touch is never examined. A record of a kind this
//! composition does not understand, or an extension key on a known record
//! whose capability is not enabled, passes untouched and stays byte for byte.
//! Presence alone is never a violation.
//!
//! Passing the audit is necessary, not sufficient: each owning capability's
//! own `validate` then runs over its changed records.

use std::fmt;

use thiserror::Error;

use crate::compose::Composition;
use crate::record::{Delta, View};
use crate::{AUTHORSHIP_KEYS, is_reserved_key};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    AuthorshipWritten {
        id: String,
        key: String,
    },
    ReservedKey {
        id: String,
        key: String,
    },
    UnclaimedKey {
        id: String,
        kind: String,
        key: String,
    },
    UnknownKindCreated {
        id: String,
        kind: String,
    },
    UnknownKindModified {
        id: String,
        kind: String,
    },
    UnknownKindDeleted {
        id: String,
        kind: String,
    },
    KindChanged {
        id: String,
        from: String,
        to: String,
    },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::AuthorshipWritten { id, key } => {
                write!(f, "{id}: {key:?} is written only by the host")
            }
            Violation::ReservedKey { id, key } => {
                write!(f, "{id}: {key:?} is reserved for the kernel")
            }
            Violation::UnclaimedKey { id, kind, key } => {
                write!(f, "{id}: no enabled capability owns {key:?} on {kind:?}")
            }
            Violation::UnknownKindCreated { id, kind } => {
                write!(
                    f,
                    "{id}: created a {kind:?} record, a kind no enabled capability owns"
                )
            }
            Violation::UnknownKindModified { id, kind } => {
                write!(
                    f,
                    "{id}: changed a {kind:?} record, a kind no enabled capability owns"
                )
            }
            Violation::UnknownKindDeleted { id, kind } => {
                write!(
                    f,
                    "{id}: deleted a {kind:?} record, a kind no enabled capability owns"
                )
            }
            Violation::KindChanged { id, from, to } => {
                write!(f, "{id}: kind changed from {from:?} to {to:?}")
            }
        }
    }
}

/// Every violation in the frame, so a refused frame explains itself fully.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub struct AuditError {
    pub violations: Vec<Violation>,
}

impl fmt::Display for AuditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "replica frame refused: ")?;
        for (index, violation) in self.violations.iter().enumerate() {
            if index > 0 {
                write!(f, "; ")?;
            }
            write!(f, "{violation}")?;
        }
        Ok(())
    }
}

/// Audit the change from `before` to `after`. Returns the delta on success.
pub fn audit(composition: &Composition, before: &View, after: &View) -> Result<Delta, AuditError> {
    let delta = Delta::between(before, after);
    let mut violations = Vec::new();

    for id in &delta.rekinded {
        let from = before.get(id).map(|r| r.kind.clone()).unwrap_or_default();
        let to = after.get(id).map(|r| r.kind.clone()).unwrap_or_default();
        violations.push(Violation::KindChanged {
            id: id.clone(),
            from,
            to,
        });
    }

    for id in &delta.created {
        let Some(record) = after.get(id) else {
            continue;
        };
        if composition.owner_of_kind(&record.kind).is_none() {
            violations.push(Violation::UnknownKindCreated {
                id: id.clone(),
                kind: record.kind.clone(),
            });
            continue;
        }
        for key in record.props.keys() {
            check_key(composition, id, &record.kind, key, &mut violations);
        }
    }

    for (id, keys) in &delta.modified {
        let Some(record) = after.get(id) else {
            continue;
        };
        if composition.owner_of_kind(&record.kind).is_none() {
            violations.push(Violation::UnknownKindModified {
                id: id.clone(),
                kind: record.kind.clone(),
            });
            continue;
        }
        for key in keys {
            check_key(composition, id, &record.kind, key, &mut violations);
        }
    }

    for id in &delta.deleted {
        let Some(record) = before.get(id) else {
            continue;
        };
        if composition.owner_of_kind(&record.kind).is_none() {
            violations.push(Violation::UnknownKindDeleted {
                id: id.clone(),
                kind: record.kind.clone(),
            });
        }
    }

    if violations.is_empty() {
        Ok(delta)
    } else {
        Err(AuditError { violations })
    }
}

fn check_key(
    composition: &Composition,
    id: &str,
    kind: &str,
    key: &str,
    violations: &mut Vec<Violation>,
) {
    if AUTHORSHIP_KEYS.contains(&key) {
        violations.push(Violation::AuthorshipWritten {
            id: id.to_string(),
            key: key.to_string(),
        });
    } else if is_reserved_key(key) {
        violations.push(Violation::ReservedKey {
            id: id.to_string(),
            key: key.to_string(),
        });
    } else if composition.writer_of(kind, key).is_none() {
        violations.push(Violation::UnclaimedKey {
            id: id.to_string(),
            kind: kind.to_string(),
            key: key.to_string(),
        });
    }
}
