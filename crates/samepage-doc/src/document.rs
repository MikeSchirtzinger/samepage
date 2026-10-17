//! The one Yrs document a peer holds.
//!
//! [`Document`] is the only type in SamePage that opens a Yrs transaction.
//! The host's authority and the browser replica both wrap it, so the layout,
//! the revision, id minting and how resolved ops become CRDT writes are one
//! implementation on both sides.

use std::sync::{Arc, Mutex};

use samepage_capability::{Minter, ResolvedOp, Scalar};
use thiserror::Error;
use yrs::updates::decoder::Decode;
use yrs::{
    Any, Doc, Map, MapPrelim, MapRef, Number, Origin, ReadTxn, StateVector, Transact,
    TransactionMut, Update,
};

use crate::layout::{self, Inspection, OBJECTS};

/// Transaction origin of every write this crate makes. Only these updates
/// are captured as outbound; updates applied from a peer are not echoed.
const LOCAL: &str = "samepage.local";
/// Observer key for the local-update capture; one per document.
const CAPTURE_OBSERVER: &str = "samepage-doc.capture";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DocError {
    #[error("update does not decode: {0}")]
    Decode(String),
    #[error("update does not apply: {0}")]
    Apply(String),
    #[error("the document is busy: {0}")]
    Busy(String),
    #[error("record {0:?} is not a map in the objects root")]
    NotARecord(String),
}

pub struct Document {
    doc: Doc,
    objects: MapRef,
    captured: Arc<Mutex<Vec<Vec<u8>>>>,
    next_seq: u32,
}

impl Document {
    /// A fresh document with a random client id.
    pub fn new() -> Self {
        Self::build(Doc::new())
    }

    pub fn with_client(client: u64) -> Self {
        Self::build(Doc::with_client_id(client))
    }

    fn build(doc: Doc) -> Self {
        let objects = doc.get_or_insert_map(OBJECTS);
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = captured.clone();
        doc.observe_update_v1(CAPTURE_OBSERVER, move |txn, event| {
            if txn.origin() == Some(&Origin::from(LOCAL))
                && let Ok(mut sink) = sink.lock()
            {
                sink.push(event.update.clone());
            }
        })
        .expect("a new document accepts an update observer");
        Self {
            doc,
            objects,
            captured,
            next_seq: 0,
        }
    }

    /// A document holding exactly the state in a full v1 update. Pending
    /// (unresolvable) content is not an error here; callers that must refuse
    /// it check [`Self::has_missing_updates`].
    pub fn from_state(update_v1: &[u8]) -> Result<Self, DocError> {
        let document = Self::new();
        document.apply_remote(decode(update_v1)?)?;
        Ok(document)
    }

    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// The objects root. Reading only; writes go through [`Self::write`].
    pub fn objects_ref(&self) -> MapRef {
        self.objects.clone()
    }

    pub fn client_id(&self) -> u64 {
        self.doc.client_id().get()
    }

    pub fn inspect(&self) -> Inspection {
        layout::inspect(&self.doc.transact())
    }

    pub fn revision(&self) -> String {
        layout::revision(&self.doc.transact())
    }

    pub fn view(&self) -> samepage_capability::View {
        let txn = self.doc.transact();
        let inspection = layout::inspect(&txn);
        layout::view(&inspection, &layout::revision(&txn))
    }

    pub fn state_vector(&self) -> StateVector {
        self.doc.transact().state_vector()
    }

    /// True when the document holds content it cannot integrate yet.
    pub fn has_missing_updates(&self) -> bool {
        self.doc.transact().has_missing_updates()
    }

    /// Everything the peer with `sv` is missing. Includes pending content if
    /// the document has any, which is why the host never lets pending
    /// content into its live document.
    pub fn encode_state(&self, sv: &StateVector) -> Vec<u8> {
        self.doc.transact().encode_state_as_update_v1(sv)
    }

    pub fn encode_full(&self) -> Vec<u8> {
        self.encode_state(&StateVector::default())
    }

    /// Apply a peer's update, not captured as outbound.
    pub fn apply_remote(&self, update: Update) -> Result<(), DocError> {
        let mut txn = self
            .doc
            .try_transact_mut()
            .map_err(|error| DocError::Busy(error.to_string()))?;
        txn.apply_update(update)
            .map_err(|error| DocError::Apply(error.to_string()))
    }

    /// A minter in this document's client namespace: `{client:016x}-{seq:08x}`.
    pub fn minter(&self) -> IdMinter {
        IdMinter {
            client: self.client_id(),
            next: self.next_seq,
        }
    }

    /// Keep the minter's position after a successful commit.
    pub fn advance(&mut self, minter: &IdMinter) {
        self.next_seq = self.next_seq.max(minter.next);
    }

    /// Run one local write transaction and return the update it produced.
    pub fn write<R>(
        &self,
        f: impl FnOnce(&mut TransactionMut, &MapRef) -> Result<R, DocError>,
    ) -> Result<(R, Vec<u8>), DocError> {
        self.take_captured();
        let result = {
            let mut txn = self
                .doc
                .try_transact_mut_with(LOCAL)
                .map_err(|error| DocError::Busy(error.to_string()))?;
            f(&mut txn, &self.objects)?
        };
        let updates = self.take_captured();
        let update = match updates.len() {
            0 => Vec::new(),
            1 => updates.into_iter().next().unwrap_or_default(),
            _ => Update::merge_updates(updates.iter().filter_map(|u| Update::decode_v1(u).ok()))
                .encode_v1(),
        };
        Ok((result, update))
    }

    fn take_captured(&self) -> Vec<Vec<u8>> {
        self.captured
            .lock()
            .map(|mut sink| std::mem::take(&mut *sink))
            .unwrap_or_default()
    }

    /// Outbound updates from local writes not yet taken.
    pub fn take_local_updates(&self) -> Vec<Vec<u8>> {
        self.take_captured()
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

use yrs::updates::encoder::Encode as _;

pub(crate) fn decode(update_v1: &[u8]) -> Result<Update, DocError> {
    Update::decode_v1(update_v1).map_err(|error| DocError::Decode(error.to_string()))
}

pub struct IdMinter {
    client: u64,
    next: u32,
}

impl Minter for IdMinter {
    fn mint(&mut self) -> String {
        let id = format!("{:016x}-{:08x}", self.client, self.next);
        self.next = self.next.wrapping_add(1);
        id
    }
}

/// The client namespace of a minted id, if it has the minted shape.
pub fn id_client(id: &str) -> Option<u64> {
    let (client, seq) = id.split_once('-')?;
    if client.len() != 16 || seq.len() != 8 {
        return None;
    }
    u32::from_str_radix(seq, 16).ok()?;
    u64::from_str_radix(client, 16).ok()
}

pub(crate) fn any(value: &Scalar) -> Any {
    match value {
        Scalar::Str(text) => Any::from(text.as_str()),
        Scalar::Num(number) => Any::Number(Number::try_i64(*number)),
        Scalar::Bool(flag) => Any::Bool(*flag),
    }
}

/// Write resolved ops inside `txn`. Resolution already checked ownership,
/// targets and references; this is the only place ops become CRDT writes.
pub(crate) fn write_ops(
    txn: &mut TransactionMut,
    objects: &MapRef,
    ops: &[ResolvedOp],
) -> Result<(), DocError> {
    for op in ops {
        match op {
            ResolvedOp::Create {
                id, kind, props, ..
            } => {
                let record = objects.insert(txn, id.as_str(), MapPrelim::default());
                record.insert(txn, "kind", Any::from(kind.as_str()));
                for (key, value) in props {
                    record.insert(txn, key.as_str(), any(value));
                }
            }
            ResolvedOp::Set { id, props, .. } => {
                let record = record(txn, objects, id)?;
                for (key, value) in props {
                    record.insert(txn, key.as_str(), any(value));
                }
            }
            ResolvedOp::Delete { id, .. } => {
                objects.remove(txn, id);
            }
        }
    }
    Ok(())
}

pub(crate) fn record(txn: &TransactionMut, objects: &MapRef, id: &str) -> Result<MapRef, DocError> {
    match objects.get(txn, id) {
        Some(yrs::Out::YMap(map)) => Ok(map),
        _ => Err(DocError::NotARecord(id.to_string())),
    }
}
