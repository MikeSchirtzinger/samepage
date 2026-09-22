//! The trait a capability implements.
//!
//! Every method is a pure function of its inputs. The same implementation is
//! linked into the native host, where agent actions call it, and exported
//! from the capability's WebAssembly artifact, where the page calls it for
//! human gestures, so both sides of the page run one piece of logic.

use serde::{Deserialize, Serialize};

use crate::descriptor::Descriptor;
use crate::plan::Plan;
use crate::record::{Delta, View};

/// One action invocation. There is no author here: who called is established
/// by the host from the authenticated caller, never taken from the call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub action: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct CapError {
    pub code: String,
    pub message: String,
}

impl CapError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// One legend key, built from the same constants the capability's renderer
/// uses, so a key cannot describe a visual the page does not draw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegendEntry {
    pub capability: String,
    /// What varies, for example `status` or `tone`.
    pub attribute: String,
    /// The value that produces this look.
    pub value: String,
    pub label: String,
    /// The class the renderer applies for this value, if any.
    #[serde(default)]
    pub class: Option<String>,
    /// A non-color mark (glyph, border style) that tells it apart.
    #[serde(default)]
    pub symbol: Option<String>,
}

pub trait Capability: Send + Sync + 'static {
    fn descriptor(&self) -> Descriptor;

    /// This capability's projection of its records, for rendering and read-back.
    fn project(&self, view: &View) -> Result<serde_json::Value, CapError>;

    /// The ops an action asks for. No side effects; the kernel resolves and
    /// commits.
    fn plan(&self, call: &Call, view: &View) -> Result<Plan, CapError>;

    /// Accept or refuse a replica's change to this capability's records. Called
    /// only with records the delta touched.
    fn validate(&self, delta: &Delta, view: &View) -> Result<(), CapError>;

    /// Ops this capability needs when records are deleted elsewhere, for
    /// example links to a removed card.
    fn cascade(&self, deleted: &[String], view: &View) -> Result<Plan, CapError> {
        let _ = (deleted, view);
        Ok(Plan::default())
    }

    /// This capability's section of the agent read-back.
    fn digest(&self, view: &View) -> Result<serde_json::Value, CapError>;

    /// Sentences describing what changed between two digests, for `reader`.
    fn changes(
        &self,
        before: &serde_json::Value,
        after: &serde_json::Value,
        reader: &str,
    ) -> Result<Vec<String>, CapError> {
        let _ = (before, after, reader);
        Ok(Vec::new())
    }

    fn legend(&self, view: &View) -> Result<Vec<LegendEntry>, CapError> {
        let _ = view;
        Ok(Vec::new())
    }
}
