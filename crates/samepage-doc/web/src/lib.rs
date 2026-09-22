//! `doc.wasm`: the page's one SamePage document replica.
//!
//! ABI `samepage.document.v1`, layout version 1. The page may hold one
//! [`Replica`] at a time: a second `new Replica()` throws until the first is
//! disposed (`free()` or `dispose()`). Every error message starts with a stable
//! code: `bad_input:`, `plan:`, `layout:`, `sync:` or `refused:`.

use std::sync::atomic::{AtomicBool, Ordering};

use samepage_capability::{Batch, Descriptor, Plan};
use samepage_doc::{ReplicaError, DOCUMENT_ABI, LAYOUT_VERSION};
use wasm_bindgen::prelude::*;

static LIVE: AtomicBool = AtomicBool::new(false);

fn err(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}

fn replica_err(error: ReplicaError) -> JsValue {
    err(error)
}

fn bad_input(what: &str, error: impl std::fmt::Display) -> JsValue {
    err(format!("bad_input: {what}: {error}"))
}

#[wasm_bindgen]
pub fn abi() -> String {
    DOCUMENT_ABI.to_string()
}

#[wasm_bindgen]
pub fn layout_version() -> u32 {
    LAYOUT_VERSION
}

#[wasm_bindgen]
pub struct Replica {
    inner: samepage_doc::Replica,
}

impl Drop for Replica {
    fn drop(&mut self) {
        LIVE.store(false, Ordering::SeqCst);
    }
}

#[wasm_bindgen]
impl Replica {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<Replica, JsValue> {
        if LIVE.swap(true, Ordering::SeqCst) {
            return Err(err(
                "samepage.document: a replica already exists in this page",
            ));
        }
        Ok(Replica {
            inner: samepage_doc::Replica::new(),
        })
    }

    /// Release this replica so a new one may be constructed.
    pub fn dispose(self) {}

    pub fn client_id(&self) -> String {
        self.inner.client_id().to_string()
    }

    pub fn set_composition(&mut self, descriptors: &str) -> Result<(), JsValue> {
        let descriptors: Vec<Descriptor> =
            serde_json::from_str(descriptors).map_err(|e| bad_input("descriptors", e))?;
        self.inner.set_composition(descriptors).map_err(replica_err)
    }

    pub fn mark_refused(&mut self, reason: &str) {
        self.inner.mark_refused(reason);
    }

    pub fn refused(&self) -> Option<String> {
        self.inner.refused().map(str::to_string)
    }

    pub fn hello(&self) -> Vec<u8> {
        self.inner.hello()
    }

    pub fn receive(&mut self, frame: &[u8]) -> Result<js_sys::Array, JsValue> {
        let replies = self.inner.receive(frame).map_err(replica_err)?;
        Ok(frames(replies))
    }

    pub fn take_outbound(&mut self) -> js_sys::Array {
        frames(self.inner.take_outbound())
    }

    pub fn take_dirty(&mut self) -> bool {
        self.inner.take_dirty()
    }

    pub fn revision(&self) -> String {
        self.inner.revision()
    }

    pub fn kinds(&self) -> String {
        serde_json::to_string(&self.inner.kinds()).unwrap_or_else(|_| "[]".to_string())
    }

    /// C1 `View` JSON of the given kinds (`[]` = all), optionally limited to ids.
    pub fn view(&self, kinds: &str, ids: Option<String>) -> Result<String, JsValue> {
        let kinds: Vec<String> = serde_json::from_str(kinds).map_err(|e| bad_input("kinds", e))?;
        let ids: Option<Vec<String>> = match ids {
            Some(text) => Some(serde_json::from_str(&text).map_err(|e| bad_input("ids", e))?),
            None => None,
        };
        let view = self.inner.view();
        let records: Vec<_> = view
            .records()
            .filter(|record| kinds.is_empty() || kinds.contains(&record.kind))
            .filter(|record| ids.as_ref().is_none_or(|ids| ids.contains(&record.id)))
            .cloned()
            .collect();
        let scoped = samepage_capability::View::new(view.revision(), records)
            .map_err(|e| err(format!("layout: {e}")))?;
        serde_json::to_string(&scoped).map_err(|e| err(format!("layout: {e}")))
    }

    pub fn snapshot(&self) -> Result<String, JsValue> {
        serde_json::to_string(&self.inner.view()).map_err(|e| err(format!("layout: {e}")))
    }

    pub fn apply(&mut self, capability: &str, plan: &str) -> Result<String, JsValue> {
        let plan: Plan = serde_json::from_str(plan).map_err(|e| bad_input("plan", e))?;
        let applied = self.inner.apply(capability, plan).map_err(replica_err)?;
        serde_json::to_string(&applied).map_err(|e| err(format!("layout: {e}")))
    }

    pub fn apply_batch(&mut self, batch: &str) -> Result<String, JsValue> {
        let batch: Batch = serde_json::from_str(batch).map_err(|e| bad_input("batch", e))?;
        let applied = self.inner.apply_batch(&batch).map_err(replica_err)?;
        serde_json::to_string(&applied).map_err(|e| err(format!("layout: {e}")))
    }
}

fn frames(frames: Vec<Vec<u8>>) -> js_sys::Array {
    frames
        .into_iter()
        .map(|frame| JsValue::from(js_sys::Uint8Array::from(frame.as_slice())))
        .collect()
}
