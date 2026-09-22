//! Plans, batch references, and all-or-nothing resolution.
//!
//! A capability never touches the document. It returns a [`Plan`]: ops that
//! name new records by a batch-local reference and existing records by id.
//! The kernel collects plans from one or more capabilities into a [`Batch`]
//! and resolves it here:
//!
//! - every `Create` gets an id from the kernel's [`Minter`], and the batch's
//!   reference table maps each reference to it;
//! - a reference must be created earlier in the same batch, by any capability
//!   in it, so one capability can link to a record another is creating;
//! - every op is checked against the composition's ownership of kinds and
//!   keys, and against the records that exist at that point in the batch;
//! - the batch must be based on the revision of the view it resolves against;
//! - no property written from a reference may survive the batch naming a
//!   record the batch deleted.
//!
//! Resolution works on a private copy. On any error nothing is returned but
//! the error, and the caller's view is never modified.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::compose::Composition;
use crate::is_reserved_key;
use crate::record::{Props, Record, Scalar, View};

/// A property value in a plan: a scalar, or the id of a record created earlier
/// in the batch, written `{"$ref": "name"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Ref {
        #[serde(rename = "$ref")]
        reference: String,
    },
    Scalar(Scalar),
}

impl Value {
    pub fn reference(name: &str) -> Self {
        Value::Ref {
            reference: name.to_string(),
        }
    }
}

impl<T: Into<Scalar>> From<T> for Value {
    fn from(value: T) -> Self {
        Value::Scalar(value.into())
    }
}

/// An existing record by id, or a record created earlier in the batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Id(String),
    Ref(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Create {
        #[serde(rename = "ref")]
        reference: String,
        kind: String,
        #[serde(default)]
        props: BTreeMap<String, Value>,
    },
    Set {
        target: Target,
        props: BTreeMap<String, Value>,
    },
    Delete {
        target: Target,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub ops: Vec<Op>,
    /// One sentence for the actor, in the capability's own words.
    #[serde(default)]
    pub readback: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchEntry {
    pub capability: String,
    pub plan: Plan,
}

/// Plans from one or more capabilities that commit together or not at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    /// The revision the plans were made against.
    pub base_revision: String,
    pub entries: Vec<BatchEntry>,
}

/// An op after resolution: ids minted, references replaced, ownership checked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ResolvedOp {
    Create {
        capability: String,
        id: String,
        kind: String,
        props: Props,
    },
    Set {
        capability: String,
        id: String,
        props: Props,
    },
    Delete {
        capability: String,
        id: String,
    },
}

/// A batch that passed resolution. Only [`Batch::resolve`] and
/// [`BatchBuilder::finish`] construct one, its fields are read-only, and it
/// cannot be deserialized, so holding a `ResolvedBatch` means this crate
/// checked it. A host must resolve an untrusted [`Batch`] itself; it never
/// accepts a resolved batch from outside. It serializes for inspection only.
///
/// It cannot be read back from JSON:
///
/// ```compile_fail,E0277
/// let forged: samepage_capability::ResolvedBatch = serde_json::from_str("{}").unwrap();
/// ```
///
/// and it cannot be built field by field outside this crate:
///
/// ```compile_fail,E0451
/// let view = samepage_capability::View::new("r", vec![]).unwrap();
/// let forged = samepage_capability::ResolvedBatch {
///     base_revision: "r".into(),
///     ops: vec![],
///     ids: Default::default(),
///     readback: vec![],
///     projected: view,
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedBatch {
    base_revision: String,
    ops: Vec<ResolvedOp>,
    ids: BTreeMap<String, String>,
    readback: Vec<String>,
    projected: View,
}

impl ResolvedBatch {
    /// The revision the batch was resolved against.
    pub fn base_revision(&self) -> &str {
        &self.base_revision
    }

    /// The checked ops, in order.
    pub fn ops(&self) -> &[ResolvedOp] {
        &self.ops
    }

    /// Batch reference -> minted id.
    pub fn ids(&self) -> &BTreeMap<String, String> {
        &self.ids
    }

    pub fn readback(&self) -> &[String] {
        &self.readback
    }

    /// The base view with this batch applied, still at the base revision. A
    /// projection for planning and checking, not a committed state.
    pub fn projected(&self) -> &View {
        &self.projected
    }

    /// Refuse to commit if the document moved since the batch was based.
    pub fn ensure_current(&self, revision: &str) -> Result<(), PlanError> {
        if self.base_revision != revision {
            return Err(PlanError::Stale {
                expected: self.base_revision.clone(),
                found: revision.to_string(),
            });
        }
        Ok(())
    }
}

/// Where new ids come from. The kernel supplies the document's own minter; a
/// minted id that exists in the base view (even if the batch deletes it), or
/// repeats one minted in the batch, fails the batch.
pub trait Minter {
    fn mint(&mut self) -> String;
}

impl<F: FnMut() -> String> Minter for F {
    fn mint(&mut self) -> String {
        self()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanError {
    #[error("the batch was planned at revision {expected:?} but the document is at {found:?}")]
    Stale { expected: String, found: String },
    #[error("capability {0:?} is not in this composition")]
    UnknownCapability(String),
    #[error("a create has an empty reference")]
    EmptyRef,
    #[error("reference {0:?} is created twice in one batch")]
    DuplicateRef(String),
    #[error("reference {0:?} is not created earlier in this batch")]
    UnknownRef(String),
    #[error("kind {0:?} has no owner in this composition")]
    UnknownKind(String),
    #[error("capability {capability:?} cannot create or delete {kind:?} records; {owner:?} owns that kind")]
    NotKindOwner {
        capability: String,
        kind: String,
        owner: String,
    },
    #[error("capability {capability:?} cannot write {key:?} on {kind:?}")]
    KeyNotWritable {
        capability: String,
        kind: String,
        key: String,
    },
    #[error("{key:?} is written only by the kernel")]
    ReservedKey { key: String },
    #[error("no record {0:?} exists at this point in the batch")]
    TargetMissing(String),
    #[error("a set on {0:?} changes nothing")]
    EmptySet(String),
    #[error("the minter returned an empty id")]
    EmptyMintedId,
    #[error("the minter returned {0:?}, which the base view or this batch already uses")]
    DuplicateMintedId(String),
    #[error("{key:?} is not a finite number")]
    NonFinite { key: String },
    #[error("{id:?}.{key} still refers to {target:?} (reference {reference:?}), which the batch deletes")]
    DanglingRef {
        id: String,
        key: String,
        target: String,
        reference: String,
    },
}

/// A property written from a batch reference, kept until [`BatchBuilder::finish`]
/// checks that it does not outlive the record it names.
#[derive(Debug, Clone)]
struct RefWrite {
    id: String,
    key: String,
    target: String,
    reference: String,
}

/// Resolves plans one capability at a time, so a coordinator can hand each
/// capability the batch so far ([`BatchBuilder::working_view`]) before asking
/// for its plan.
#[derive(Debug, Clone)]
pub struct BatchBuilder<'a> {
    composition: &'a Composition,
    base_revision: String,
    working: View,
    refs: BTreeMap<String, String>,
    minted: BTreeSet<String>,
    ref_writes: Vec<RefWrite>,
    ops: Vec<ResolvedOp>,
    readback: Vec<String>,
}

impl<'a> BatchBuilder<'a> {
    /// Start from a copy of `view`. The caller's view is never modified.
    ///
    /// Every id in the base view is reserved for the whole batch: a record
    /// deleted in the batch does not free its id for a new record, because
    /// replicas that saw the old record would read the new one as the same
    /// object.
    pub fn new(composition: &'a Composition, view: &View) -> Self {
        Self {
            composition,
            base_revision: view.revision().to_string(),
            working: view.clone(),
            refs: BTreeMap::new(),
            minted: view.records().map(|record| record.id.clone()).collect(),
            ref_writes: Vec::new(),
            ops: Vec::new(),
            readback: Vec::new(),
        }
    }

    /// The base view with every accepted plan applied so far.
    pub fn working_view(&self) -> &View {
        &self.working
    }

    /// Reference -> id for everything created so far.
    pub fn ids(&self) -> &BTreeMap<String, String> {
        &self.refs
    }

    /// Resolve one capability's plan. On error the builder is unchanged, so a
    /// refused plan never leaves half of itself behind.
    ///
    /// A later op or plan in the batch may still delete a record that an
    /// earlier reference names; that is caught by [`Self::finish`], which
    /// checks the batch as a whole.
    pub fn push(
        &mut self,
        capability: &str,
        plan: &Plan,
        minter: &mut dyn Minter,
    ) -> Result<(), PlanError> {
        if self.composition.descriptor(capability).is_none() {
            return Err(PlanError::UnknownCapability(capability.to_string()));
        }
        let mut next = self.clone();
        for op in &plan.ops {
            next.apply(capability, op, minter)?;
        }
        if !plan.readback.is_empty() {
            next.readback.push(plan.readback.clone());
        }
        *self = next;
        Ok(())
    }

    /// Close the batch. Refused if any property written from a reference
    /// survives to the end of the batch naming a record the batch deleted.
    /// A reference whose record was itself deleted, or whose key was later
    /// overwritten with another value, no longer points anywhere and passes.
    pub fn finish(self) -> Result<ResolvedBatch, PlanError> {
        for write in &self.ref_writes {
            let Some(record) = self.working.get(&write.id) else {
                continue;
            };
            if record.props.get(&write.key) != Some(&Scalar::Str(write.target.clone())) {
                continue;
            }
            if !self.working.contains(&write.target) {
                return Err(PlanError::DanglingRef {
                    id: write.id.clone(),
                    key: write.key.clone(),
                    target: write.target.clone(),
                    reference: write.reference.clone(),
                });
            }
        }
        Ok(ResolvedBatch {
            base_revision: self.base_revision,
            ops: self.ops,
            ids: self.refs,
            readback: self.readback,
            projected: self.working,
        })
    }

    fn apply(
        &mut self,
        capability: &str,
        op: &Op,
        minter: &mut dyn Minter,
    ) -> Result<(), PlanError> {
        match op {
            Op::Create {
                reference,
                kind,
                props,
            } => {
                if reference.is_empty() {
                    return Err(PlanError::EmptyRef);
                }
                if self.refs.contains_key(reference) {
                    return Err(PlanError::DuplicateRef(reference.clone()));
                }
                self.require_kind_owner(capability, kind)?;
                let (props, refs) = self.resolve_props(capability, kind, props)?;
                let id = minter.mint();
                if id.is_empty() {
                    return Err(PlanError::EmptyMintedId);
                }
                if self.working.contains(&id) || self.minted.contains(&id) {
                    return Err(PlanError::DuplicateMintedId(id));
                }
                self.minted.insert(id.clone());
                self.record_refs(&id, refs);
                self.refs.insert(reference.clone(), id.clone());
                self.working.insert(Record {
                    id: id.clone(),
                    kind: kind.clone(),
                    props: props.clone(),
                });
                self.ops.push(ResolvedOp::Create {
                    capability: capability.to_string(),
                    id,
                    kind: kind.clone(),
                    props,
                });
            }
            Op::Set { target, props } => {
                let id = self.target(target)?;
                let kind = self
                    .working
                    .get(&id)
                    .map(|record| record.kind.clone())
                    .ok_or_else(|| PlanError::TargetMissing(id.clone()))?;
                if self.composition.owner_of_kind(&kind).is_none() {
                    return Err(PlanError::UnknownKind(kind));
                }
                if props.is_empty() {
                    return Err(PlanError::EmptySet(id));
                }
                let (props, refs) = self.resolve_props(capability, &kind, props)?;
                self.record_refs(&id, refs);
                if let Some(record) = self.working.get_mut(&id) {
                    record.props.extend(props.clone());
                }
                self.ops.push(ResolvedOp::Set {
                    capability: capability.to_string(),
                    id,
                    props,
                });
            }
            Op::Delete { target } => {
                let id = self.target(target)?;
                let kind = self
                    .working
                    .get(&id)
                    .map(|record| record.kind.clone())
                    .ok_or_else(|| PlanError::TargetMissing(id.clone()))?;
                self.require_kind_owner(capability, &kind)?;
                self.working.remove(&id);
                self.ops.push(ResolvedOp::Delete {
                    capability: capability.to_string(),
                    id,
                });
            }
        }
        Ok(())
    }

    fn require_kind_owner(&self, capability: &str, kind: &str) -> Result<(), PlanError> {
        match self.composition.owner_of_kind(kind) {
            None => Err(PlanError::UnknownKind(kind.to_string())),
            Some(owner) if owner != capability => Err(PlanError::NotKindOwner {
                capability: capability.to_string(),
                kind: kind.to_string(),
                owner: owner.to_string(),
            }),
            Some(_) => Ok(()),
        }
    }

    fn target(&self, target: &Target) -> Result<String, PlanError> {
        let id = match target {
            Target::Id(id) => id.clone(),
            Target::Ref(reference) => self
                .refs
                .get(reference)
                .cloned()
                .ok_or_else(|| PlanError::UnknownRef(reference.clone()))?,
        };
        if !self.working.contains(&id) {
            return Err(PlanError::TargetMissing(id));
        }
        Ok(id)
    }

    fn record_refs(&mut self, id: &str, refs: Vec<(String, String, String)>) {
        for (key, target, reference) in refs {
            self.ref_writes.push(RefWrite {
                id: id.to_string(),
                key,
                target,
                reference,
            });
        }
    }

    /// Resolved props, plus (key, target id, reference) for every reference.
    #[allow(clippy::type_complexity)]
    fn resolve_props(
        &self,
        capability: &str,
        kind: &str,
        props: &BTreeMap<String, Value>,
    ) -> Result<(Props, Vec<(String, String, String)>), PlanError> {
        let mut resolved = Props::new();
        let mut refs = Vec::new();
        for (key, value) in props {
            if is_reserved_key(key) {
                return Err(PlanError::ReservedKey { key: key.clone() });
            }
            if !self.composition.may_write(capability, kind, key) {
                return Err(PlanError::KeyNotWritable {
                    capability: capability.to_string(),
                    kind: kind.to_string(),
                    key: key.clone(),
                });
            }
            let scalar = match value {
                Value::Scalar(scalar) => {
                    if !scalar.is_finite() {
                        return Err(PlanError::NonFinite { key: key.clone() });
                    }
                    scalar.clone()
                }
                Value::Ref { reference } => {
                    let id = self
                        .refs
                        .get(reference)
                        .cloned()
                        .ok_or_else(|| PlanError::UnknownRef(reference.clone()))?;
                    // A reference to a record deleted earlier in the batch
                    // would commit a dangling id.
                    if !self.working.contains(&id) {
                        return Err(PlanError::TargetMissing(id));
                    }
                    refs.push((key.clone(), id.clone(), reference.clone()));
                    Scalar::Str(id)
                }
            };
            resolved.insert(key.clone(), scalar);
        }
        Ok((resolved, refs))
    }
}

impl Batch {
    /// Resolve every entry in order against `view`, all or nothing.
    pub fn resolve(
        &self,
        composition: &Composition,
        view: &View,
        minter: &mut dyn Minter,
    ) -> Result<ResolvedBatch, PlanError> {
        if self.base_revision != view.revision() {
            return Err(PlanError::Stale {
                expected: self.base_revision.clone(),
                found: view.revision().to_string(),
            });
        }
        let mut builder = BatchBuilder::new(composition, view);
        for entry in &self.entries {
            builder.push(&entry.capability, &entry.plan, minter)?;
        }
        builder.finish()
    }
}
