//! The JSON calling convention of a capability's WebAssembly exports.
//!
//! Every export takes JSON strings and returns one JSON string:
//! `{"abi": "samepage.capability.v2", "ok": <result>}` or
//! `{"abi": "samepage.capability.v2", "error": {"code": ..., "message": ...}}`.
//! Input that does not parse is an error with code `bad_input`, never a panic
//! across the boundary. A result holding a NaN or infinite [`Scalar`](crate::Scalar)
//! is an error with code `non_finite_output`, never an `ok` with the number
//! replaced. These functions are the whole implementation of the
//! exports, so the convention is tested natively; `export_capability!` only
//! forwards to them.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::ABI;
use crate::capability::{Call, CapError, Capability};
use crate::record::{Delta, NON_FINITE_SERIALIZE, View};

fn ok(value: impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(value) => json!({ "abi": ABI, "ok": value }).to_string(),
        Err(error) if error.to_string().contains(NON_FINITE_SERIALIZE) => failure(&CapError::new(
            "non_finite_output",
            "the capability produced a NaN or infinite number; nothing was returned",
        )),
        Err(error) => failure(&CapError::new("encode", error.to_string())),
    }
}

fn failure(error: &CapError) -> String {
    json!({ "abi": ABI, "error": { "code": error.code, "message": error.message } }).to_string()
}

fn finish<T: Serialize>(result: Result<T, CapError>) -> String {
    match result {
        Ok(value) => ok(value),
        Err(error) => failure(&error),
    }
}

fn input<T: DeserializeOwned>(name: &str, text: &str) -> Result<T, CapError> {
    serde_json::from_str(text)
        .map_err(|error| CapError::new("bad_input", format!("{name}: {error}")))
}

pub fn descriptor(capability: &dyn Capability) -> String {
    ok(capability.descriptor())
}

pub fn project(capability: &dyn Capability, view: &str) -> String {
    finish(input::<View>("view", view).and_then(|view| capability.project(&view)))
}

/// Under ABI v2 `plan` answers with a [`Batch`](crate::Batch), never a bare
/// [`Plan`](crate::Plan): it calls [`Capability::plan_batch`], whose default
/// wraps the capability's own `plan` in one self-owned entry. There is one
/// planning export and one planning schema.
pub fn plan(capability: &dyn Capability, call: &str, view: &str) -> String {
    finish((|| {
        let call: Call = input("call", call)?;
        let view: View = input("view", view)?;
        capability.plan_batch(&call, &view)
    })())
}

pub fn validate(capability: &dyn Capability, delta: &str, view: &str) -> String {
    finish((|| {
        let delta: Delta = input("delta", delta)?;
        let view: View = input("view", view)?;
        capability.validate(&delta, &view)
    })())
}

pub fn cascade(capability: &dyn Capability, deleted: &str, view: &str) -> String {
    finish((|| {
        let deleted: Vec<String> = input("deleted", deleted)?;
        let view: View = input("view", view)?;
        capability.cascade(&deleted, &view)
    })())
}

pub fn digest(capability: &dyn Capability, view: &str) -> String {
    finish(input::<View>("view", view).and_then(|view| capability.digest(&view)))
}

pub fn changes(capability: &dyn Capability, before: &str, after: &str, reader: &str) -> String {
    finish((|| {
        let before: serde_json::Value = input("before", before)?;
        let after: serde_json::Value = input("after", after)?;
        capability.changes(&before, &after, reader)
    })())
}

pub fn legend(capability: &dyn Capability, view: &str) -> String {
    finish(input::<View>("view", view).and_then(|view| capability.legend(&view)))
}

/// Read an envelope back: the `ok` value as `T`, or the error it carries. An
/// envelope from another ABI version is refused.
pub fn decode<T: DeserializeOwned>(envelope: &str) -> Result<T, CapError> {
    let value: serde_json::Value = input("envelope", envelope)?;
    if value.get("abi").and_then(|abi| abi.as_str()) != Some(ABI) {
        return Err(CapError::new(
            "abi_mismatch",
            format!("envelope is not {ABI}: {envelope}"),
        ));
    }
    if let Some(error) = value.get("error") {
        let error: CapError = serde_json::from_value(error.clone())
            .map_err(|e| CapError::new("bad_input", format!("error: {e}")))?;
        return Err(error);
    }
    let ok = value
        .get("ok")
        .cloned()
        .ok_or_else(|| CapError::new("bad_input", "envelope has neither ok nor error"))?;
    serde_json::from_value(ok).map_err(|e| CapError::new("bad_input", format!("ok: {e}")))
}
