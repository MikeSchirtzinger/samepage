//! The trait a capability implements.
//!
//! Every method is a pure function of its inputs. The same implementation is
//! linked into the native host, where agent actions call it, and exported
//! from the capability's WebAssembly artifact, where the page calls it for
//! human gestures, so both sides of the page run one piece of logic.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::descriptor::Descriptor;
use crate::plan::{Batch, BatchEntry, Plan};
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

/// The kind of caller the host authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallerKind {
    Human,
    Agent,
}

/// Who made a change, as the host authenticated it. Built only by the host
/// from the authenticated connection or dispatcher caller; never read from a
/// frame, plan, batch or action argument. Decide on `participant`, not on
/// `label`: two participants may share a display name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    pub participant: String,
    pub caller: CallerKind,
    #[serde(default)]
    pub label: Option<String>,
}

/// The route a change took into the document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "route", rename_all = "snake_case")]
pub enum Origin {
    /// An action the host dispatched: the capability that planned it and the
    /// action's preferred name (also when it was called by an alias).
    HostAction { capability: String, action: String },
    /// A batch handed to the host from outside and resolved there. Its plans
    /// were not made by the host, so nothing about it is host-verified.
    UntrustedBatch,
    /// A browser replica's CRDT frame, including held frames released later.
    ReplicaFrame,
}

/// Everything the host knows about one change, for [`Capability::validate_change`].
#[derive(Debug, Clone, Copy)]
pub struct Change<'a> {
    pub principal: &'a Principal,
    pub origin: &'a Origin,
    /// The document before the change.
    pub before: &'a View,
    /// The document after the change (not yet committed).
    pub after: &'a View,
    /// What changed in value.
    pub delta: &'a Delta,
    /// Record id -> keys the change wrote, including writes whose value equals
    /// the old one, which the delta does not show.
    pub attempted: &'a BTreeMap<String, BTreeSet<String>>,
    /// Facts this capability's registered native verifier computed for this
    /// change, or `None` when no verifier is registered. A capability that
    /// needs facts must refuse when they are missing.
    pub facts: Option<&'a serde_json::Value>,
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

    /// One action, one atomic batch.
    ///
    /// The default wraps [`Self::plan`] in a single entry owned by this
    /// capability, so a capability that writes only its own kinds needs
    /// nothing else. A capability that composes another's records (a typed
    /// diagram creates cards and links before it can type them) overrides
    /// this and returns ordered entries, using batch references to point at
    /// the records earlier entries create.
    ///
    /// It is pure, like `plan`: no side effects, no host resources. The host
    /// checks the batch before anything is written, and an entry naming
    /// another capability is routing only: that owner's ownership rules and
    /// its own `validate_change` still decide.
    fn plan_batch(&self, call: &Call, view: &View) -> Result<Batch, CapError> {
        Ok(Batch {
            base_revision: view.revision().to_string(),
            entries: vec![BatchEntry {
                capability: self.descriptor().id,
                plan: self.plan(call, view)?,
            }],
        })
    }

    /// **Host authority hook.** Called only by the native host, for every
    /// change that writes (or attempts to write, even with an unchanged value)
    /// this capability's kinds or keys, whatever the route: host action,
    /// untrusted batch or replica frame. This is where policy that depends on
    /// who is writing, on the route, or on the state before the change
    /// belongs. The browser never calls it and makes no authority decision;
    /// the wasm `validate` export is a structural pre-check only.
    ///
    /// The default applies the structural `validate`.
    fn validate_change(&self, change: &Change<'_>) -> Result<(), CapError> {
        self.validate(change.delta, change.after)
    }

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
