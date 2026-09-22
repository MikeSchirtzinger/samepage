//! What a capability declares about itself.
//!
//! A descriptor is data, not code, so the same declaration is compiled into
//! the host and returned by the capability's WebAssembly artifact, and the two
//! can be compared.
//!
//! [`Composition::new`](crate::Composition::new) checks, and refuses the set
//! on any failure:
//!
//! - `abi` equals [`ABI`](crate::ABI); `id` is lowercase words joined by `-`
//!   and unique; `version` is non-empty;
//! - `depends_on`: each dependency is present, earlier in the list (so there
//!   are no cycles), not the capability itself, and at the exact `version`;
//! - `kinds`: each kind name is well formed and has one owner; each key is a
//!   lowercase snake_case name, not reserved, and has one writer;
//! - `props`: the kind has an owner, the owner is a direct or transitive
//!   dependency and not the claimant, and each key follows the key rules;
//! - `reads`: each kind is owned by a dependency;
//! - `actions`: every name and alias is lowercase `noun_verb`, public names do
//!   not start with a product prefix, and no name or alias repeats anywhere in
//!   the set;
//! - `artifact`: `NativeOnly` has a non-empty reason and claims no kinds or
//!   props.
//!
//! Composition does **not** check, and later stages must:
//!
//! - `input_schema` validity and argument validation: the host dispatcher
//!   compiles each schema at startup and validates every call (C2/C3);
//! - `triggers` and the artifact itself (hash, size, ABI, and that the
//!   artifact's own descriptor equals the host's): the browser loader and
//!   manifest (C3);
//! - `schema` against persisted documents: the kernel on open (C2);
//! - `visual_actions` names and who may invoke them: the capability that
//!   consumes them (D2, walkthroughs);
//! - `audience`, `effect` and `description` semantics, and the `reason` text of
//!   kind, prop and dependency claims: review, not code. `effect` is not yet
//!   enforced against what a plan actually does.

use serde::{Deserialize, Serialize};

use crate::ABI;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub abi: String,
    /// Stable lowercase id, for example `map` or `sources`.
    pub id: String,
    /// Exact version. Dependencies name an exact version too, so a composition
    /// cannot pair a capability with a dependency it was not built against.
    pub version: String,
    /// Kinds this capability owns, each with the exact property keys it owns
    /// on that kind. There is no wildcard: a key not listed here or in some
    /// capability's [`PropClaim`] has no writer, so plans cannot set it and a
    /// replica frame that changes it is refused.
    #[serde(default)]
    pub kinds: Vec<KindClaim>,
    /// Extension keys this capability owns on a kind a dependency owns.
    #[serde(default)]
    pub props: Vec<PropClaim>,
    /// Kinds this capability may see but never writes. Each must be owned by a
    /// dependency.
    #[serde(default)]
    pub reads: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<Dep>,
    #[serde(default)]
    pub actions: Vec<ActionDecl>,
    /// Named visual behaviors other capabilities may invoke, for example a
    /// walkthrough beat's `trace-path`. Not checked by composition.
    #[serde(default)]
    pub visual_actions: Vec<String>,
    /// When the browser loads this capability's artifact. Checked by the loader,
    /// not by composition.
    #[serde(default)]
    pub triggers: Vec<Trigger>,
    pub artifact: Artifact,
    /// Schema version of the kinds and keys this capability owns. Checked by the
    /// kernel against persisted documents, not by composition.
    pub schema: u32,
}

impl Descriptor {
    /// A descriptor at the current [`ABI`] with nothing declared yet.
    pub fn new(id: impl Into<String>, version: impl Into<String>, artifact: Artifact) -> Self {
        Self {
            abi: ABI.to_string(),
            id: id.into(),
            version: version.into(),
            kinds: Vec::new(),
            props: Vec::new(),
            reads: Vec::new(),
            depends_on: Vec::new(),
            actions: Vec::new(),
            visual_actions: Vec::new(),
            triggers: Vec::new(),
            artifact,
            schema: 1,
        }
    }

    pub fn owns(mut self, kind: &str, keys: &[&str], reason: &str) -> Self {
        self.kinds.push(KindClaim {
            kind: kind.to_string(),
            keys: keys.iter().map(|key| key.to_string()).collect(),
            reason: reason.to_string(),
        });
        self
    }

    pub fn extends(mut self, kind: &str, keys: &[&str], reason: &str) -> Self {
        self.props.push(PropClaim {
            kind: kind.to_string(),
            keys: keys.iter().map(|key| key.to_string()).collect(),
            reason: reason.to_string(),
        });
        self
    }

    pub fn reads(mut self, kind: &str) -> Self {
        self.reads.push(kind.to_string());
        self
    }

    pub fn depends_on(mut self, id: &str, version: &str, reason: &str) -> Self {
        self.depends_on.push(Dep {
            id: id.to_string(),
            version: version.to_string(),
            reason: reason.to_string(),
        });
        self
    }

    pub fn action(mut self, action: ActionDecl) -> Self {
        self.actions.push(action);
        self
    }

    pub fn trigger(mut self, trigger: Trigger) -> Self {
        self.triggers.push(trigger);
        self
    }

    /// Whether this capability claims any part of the shared document.
    pub fn claims_document(&self) -> bool {
        !self.kinds.is_empty() || !self.props.is_empty()
    }
}

/// Ownership of one kind: the kind itself (only the owner creates or deletes
/// its records) and exactly the listed keys on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindClaim {
    pub kind: String,
    pub keys: Vec<String>,
    pub reason: String,
}

/// Ownership of extension keys on a kind that a dependency owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PropClaim {
    pub kind: String,
    pub keys: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dep {
    pub id: String,
    pub version: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    Human,
    Agent,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Query,
    Mutate,
}

/// One action. `name` is the public name. `aliases` are older names that
/// still dispatch to it and are never listed or documented.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDecl {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub audience: Audience,
    pub effect: Effect,
    pub description: String,
    pub input_schema: serde_json::Value,
}

impl ActionDecl {
    pub fn new(name: &str, audience: Audience, effect: Effect, description: &str) -> Self {
        Self {
            name: name.to_string(),
            aliases: Vec::new(),
            audience,
            effect,
            description: description.to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
        }
    }

    pub fn alias(mut self, alias: &str) -> Self {
        self.aliases.push(alias.to_string());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "on", content = "name", rename_all = "snake_case")]
pub enum Trigger {
    /// Loaded with the page.
    Always,
    /// Loaded when the document holds a record of this kind.
    Kind(String),
    /// Loaded when this tool is picked.
    Tool(String),
    /// Loaded when this route is opened.
    Route(String),
}

/// How the capability runs in the browser.
///
/// `NativeOnly` is for host adapters (for example a process-running service)
/// and is refused by composition for any capability that claims part of the
/// document: owning records means having portable logic that projects and
/// validates them, and that logic ships as WebAssembly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Artifact {
    Wasm,
    NativeOnly { reason: String },
}
