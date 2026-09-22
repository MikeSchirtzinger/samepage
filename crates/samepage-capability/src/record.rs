//! The flat record view of the shared document.
//!
//! The document is a map of objects, each a flat map of scalar properties with
//! a `kind`. A [`View`] is that shape with the CRDT taken away: the kernel
//! builds one from the live document, and a capability reads only records.
//! Structured values are stored as JSON strings, exactly as the document
//! stores them, so a view never invents a type the document does not have.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize, Serializer};
use thiserror::Error;

use crate::KIND_KEY;

/// The message a non-finite [`Scalar`] fails to serialize with. The envelope
/// recognizes it and reports `non_finite_output`.
pub(crate) const NON_FINITE_SERIALIZE: &str = "samepage: refusing to serialize a non-finite number";

/// One property value. Numbers must be finite: the document has no encoding
/// for NaN or infinity, and a view that held one would describe a document
/// that cannot exist.
///
/// Serializing a non-finite number is an error rather than JSON `null`
/// (serde_json's default), so a computed NaN can never leave a capability as
/// a successful result with a silently changed value.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Scalar {
    Bool(bool),
    Num(f64),
    Str(String),
}

impl Serialize for Scalar {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Scalar::Bool(value) => serializer.serialize_bool(*value),
            Scalar::Num(value) if value.is_finite() => serializer.serialize_f64(*value),
            Scalar::Num(_) => Err(serde::ser::Error::custom(NON_FINITE_SERIALIZE)),
            Scalar::Str(value) => serializer.serialize_str(value),
        }
    }
}

impl Scalar {
    pub fn is_finite(&self) -> bool {
        match self {
            Scalar::Num(value) => value.is_finite(),
            Scalar::Bool(_) | Scalar::Str(_) => true,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Scalar::Str(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Scalar::Num(value) => Some(*value),
            _ => None,
        }
    }
}

impl From<&str> for Scalar {
    fn from(value: &str) -> Self {
        Scalar::Str(value.to_string())
    }
}

impl From<String> for Scalar {
    fn from(value: String) -> Self {
        Scalar::Str(value)
    }
}

impl From<f64> for Scalar {
    fn from(value: f64) -> Self {
        Scalar::Num(value)
    }
}

impl From<bool> for Scalar {
    fn from(value: bool) -> Self {
        Scalar::Bool(value)
    }
}

pub type Props = BTreeMap<String, Scalar>;

/// One object. `kind` is structural and lives beside the properties, never
/// inside them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub props: Props,
}

impl Record {
    pub fn new(id: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            props: Props::new(),
        }
    }

    pub fn with(mut self, key: impl Into<String>, value: impl Into<Scalar>) -> Self {
        self.props.insert(key.into(), value.into());
        self
    }

    fn check(&self) -> Result<(), RecordError> {
        if self.id.is_empty() {
            return Err(RecordError::EmptyId);
        }
        if self.kind.is_empty() {
            return Err(RecordError::EmptyKind {
                id: self.id.clone(),
            });
        }
        for (key, value) in &self.props {
            if key == KIND_KEY {
                return Err(RecordError::KindInProps {
                    id: self.id.clone(),
                });
            }
            if !value.is_finite() {
                return Err(RecordError::NonFinite {
                    id: self.id.clone(),
                    key: key.clone(),
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RecordError {
    #[error("a record has an empty id")]
    EmptyId,
    #[error("record {id:?} has an empty kind")]
    EmptyKind { id: String },
    #[error("record {id:?} carries `kind` as a property; kind is structural")]
    KindInProps { id: String },
    #[error("record {id:?} has a non-finite number at {key:?}")]
    NonFinite { id: String, key: String },
    #[error("two records share the id {0:?}")]
    DuplicateId(String),
}

/// The document as records, at one revision.
///
/// Records are keyed by id. Construction refuses duplicate ids, empty ids or
/// kinds, `kind` inside props, and non-finite numbers, so every `View` a
/// capability receives describes a document that could exist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ViewWire", into = "ViewWire")]
pub struct View {
    revision: String,
    records: BTreeMap<String, Record>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewWire {
    revision: String,
    records: Vec<Record>,
}

impl TryFrom<ViewWire> for View {
    type Error = RecordError;

    fn try_from(wire: ViewWire) -> Result<Self, Self::Error> {
        View::new(wire.revision, wire.records)
    }
}

impl From<View> for ViewWire {
    fn from(view: View) -> Self {
        ViewWire {
            revision: view.revision,
            records: view.records.into_values().collect(),
        }
    }
}

impl View {
    pub fn new(
        revision: impl Into<String>,
        records: impl IntoIterator<Item = Record>,
    ) -> Result<Self, RecordError> {
        let mut map = BTreeMap::new();
        for record in records {
            record.check()?;
            let id = record.id.clone();
            if map.insert(id.clone(), record).is_some() {
                return Err(RecordError::DuplicateId(id));
            }
        }
        Ok(Self {
            revision: revision.into(),
            records: map,
        })
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn get(&self, id: &str) -> Option<&Record> {
        self.records.get(id)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.records.contains_key(id)
    }

    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.records.values()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The records of these kinds only, at the same revision. The kernel hands
    /// a capability the kinds it owns or declared it reads, nothing else.
    pub fn of_kinds(&self, kinds: &[&str]) -> View {
        View {
            revision: self.revision.clone(),
            records: self
                .records
                .iter()
                .filter(|(_, record)| kinds.contains(&record.kind.as_str()))
                .map(|(id, record)| (id.clone(), record.clone()))
                .collect(),
        }
    }

    pub(crate) fn insert(&mut self, record: Record) {
        self.records.insert(record.id.clone(), record);
    }

    pub(crate) fn get_mut(&mut self, id: &str) -> Option<&mut Record> {
        self.records.get_mut(id)
    }

    pub(crate) fn remove(&mut self, id: &str) -> Option<Record> {
        self.records.remove(id)
    }
}

/// What changed between two views, by record and by key.
///
/// `modified` lists every key whose value changed, appeared or disappeared.
/// A record whose `kind` changed under the same id is listed in `rekinded`
/// rather than `modified`, because no capability can own that change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delta {
    pub created: BTreeSet<String>,
    pub modified: BTreeMap<String, BTreeSet<String>>,
    pub deleted: BTreeSet<String>,
    pub rekinded: BTreeSet<String>,
}

impl Delta {
    pub fn between(before: &View, after: &View) -> Delta {
        let mut delta = Delta::default();
        for (id, next) in &after.records {
            let Some(prior) = before.records.get(id) else {
                delta.created.insert(id.clone());
                continue;
            };
            if prior.kind != next.kind {
                delta.rekinded.insert(id.clone());
                continue;
            }
            let keys: BTreeSet<String> = prior
                .props
                .keys()
                .chain(next.props.keys())
                .filter(|key| prior.props.get(*key) != next.props.get(*key))
                .cloned()
                .collect();
            if !keys.is_empty() {
                delta.modified.insert(id.clone(), keys);
            }
        }
        for id in before.records.keys() {
            if !after.records.contains_key(id) {
                delta.deleted.insert(id.clone());
            }
        }
        delta
    }

    pub fn is_empty(&self) -> bool {
        self.created.is_empty()
            && self.modified.is_empty()
            && self.deleted.is_empty()
            && self.rekinded.is_empty()
    }
}
