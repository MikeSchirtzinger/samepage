//! The browser's replica.
//!
//! The page holds exactly one. It applies what the host sends, plans nothing
//! itself (capabilities do), resolves a capability's plan against its own
//! current view with the same C1 rules the host uses, writes the ops locally
//! and queues the update for the host. It never writes authorship: the host
//! stamps it, and the stamp arrives back as part of the commit broadcast.

use samepage_capability::{
    Batch, BatchBuilder, BatchEntry, Composition, Descriptor, Plan, PlanError, View,
};
use serde::Serialize;
use thiserror::Error;

use crate::document::{self, decode, DocError, Document};
use crate::sync::{self, Incoming, SyncError};

#[derive(Debug, Clone, PartialEq, Error)]
pub enum ReplicaError {
    #[error("bad_input: {0}")]
    BadInput(String),
    #[error("plan: {0}")]
    Plan(#[from] PlanError),
    #[error("plan: no composition set; call set_composition first")]
    NoComposition,
    #[error("layout: {0}")]
    Doc(#[from] DocError),
    #[error("sync: {0}")]
    Sync(#[from] SyncError),
    #[error("refused: {0}")]
    Refused(String),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Applied {
    pub revision: String,
    pub ids: std::collections::BTreeMap<String, String>,
}

pub struct Replica {
    document: Document,
    composition: Option<Composition>,
    outbound: Vec<Vec<u8>>,
    dirty: bool,
    refused: Option<String>,
}

impl Default for Replica {
    fn default() -> Self {
        Self::new()
    }
}

impl Replica {
    pub fn new() -> Self {
        Self {
            document: Document::new(),
            composition: None,
            outbound: Vec::new(),
            dirty: false,
            refused: None,
        }
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn client_id(&self) -> u64 {
        self.document.client_id()
    }

    pub fn set_composition(&mut self, descriptors: Vec<Descriptor>) -> Result<(), ReplicaError> {
        let composition = Composition::new(descriptors)
            .map_err(|error| ReplicaError::BadInput(error.to_string()))?;
        self.composition = Some(composition);
        Ok(())
    }

    fn check_refused(&self) -> Result<(), ReplicaError> {
        match &self.refused {
            Some(reason) => Err(ReplicaError::Refused(reason.clone())),
            None => Ok(()),
        }
    }

    /// The host refused this replica's update. From now on it only reports
    /// that; the page must dispose of it and reconnect with a new replica.
    pub fn mark_refused(&mut self, reason: impl Into<String>) {
        self.refused = Some(reason.into());
        self.outbound.clear();
    }

    pub fn refused(&self) -> Option<&str> {
        self.refused.as_deref()
    }

    pub fn hello(&self) -> Vec<u8> {
        sync::step1(&self.document.state_vector())
    }

    /// Apply a frame from the host; returns frames to send back.
    pub fn receive(&mut self, frame: &[u8]) -> Result<Vec<Vec<u8>>, ReplicaError> {
        let _span = samepage_capability::activity::Span::new("doc", "doc.sync.receive");
        self.check_refused()?;
        let mut replies = Vec::new();
        for message in sync::parse(frame)? {
            match message {
                Incoming::Step1(sv) => replies.push(sync::step2(self.document.encode_state(&sv))),
                Incoming::Update(update) => {
                    self.document.apply_remote(decode(&update)?)?;
                    self.dirty = true;
                }
            }
        }
        Ok(replies)
    }

    pub fn take_outbound(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.outbound)
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn revision(&self) -> String {
        self.document.revision()
    }

    pub fn view(&self) -> View {
        self.document.view()
    }

    pub fn kinds(&self) -> Vec<String> {
        let mut kinds: Vec<String> = self
            .view()
            .records()
            .map(|record| record.kind.clone())
            .collect();
        kinds.sort();
        kinds.dedup();
        kinds
    }

    /// Resolve one capability's plan at the current revision and apply it.
    pub fn apply(&mut self, capability: &str, plan: Plan) -> Result<Applied, ReplicaError> {
        let batch = Batch {
            base_revision: self.revision(),
            entries: vec![BatchEntry {
                capability: capability.to_string(),
                plan,
            }],
        };
        self.apply_batch(&batch)
    }

    pub fn apply_batch(&mut self, batch: &Batch) -> Result<Applied, ReplicaError> {
        let _span = samepage_capability::activity::Span::new("doc", "doc.batch.apply");
        self.check_refused()?;
        let composition = self
            .composition
            .as_ref()
            .ok_or(ReplicaError::NoComposition)?;
        let view = self.document.view();
        if batch.base_revision != view.revision() {
            return Err(PlanError::Stale {
                expected: batch.base_revision.clone(),
                found: view.revision().to_string(),
            }
            .into());
        }
        let mut minter = self.document.minter();
        let _resolve = samepage_capability::activity::Span::new("doc", "doc.batch.resolve");
        let mut builder = BatchBuilder::new(composition, &view);
        for entry in &batch.entries {
            builder.push(&entry.capability, &entry.plan, &mut minter)?;
        }
        let resolved = builder.finish()?;
        let ops = resolved.ops().to_vec();
        let ((), update) = self
            .document
            .write(|txn, objects| document::write_ops(txn, objects, &ops))?;
        self.document.advance(&minter);
        if !update.is_empty() {
            self.outbound.push(sync::update(update));
        }
        self.dirty = true;
        Ok(Applied {
            revision: self.document.revision(),
            ids: resolved.ids().clone(),
        })
    }
}
