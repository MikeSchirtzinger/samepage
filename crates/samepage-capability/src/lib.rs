//! The contract every SamePage capability is built against.
//!
//! One shared CRDT document holds every object a page contains. Capabilities
//! are separately built units (a native `rlib` in the host, a WebAssembly
//! artifact in the browser) that each own part of that document. This crate
//! is the part they agree on, and it deliberately knows nothing about the
//! CRDT library or the transport:
//!
//! - [`descriptor`]: what a capability declares: the kinds it owns and exactly
//!   which property keys, extension keys it writes on another capability's
//!   kind, dependencies, actions and their legacy aliases, load triggers, and
//!   its artifact.
//! - [`compose`]: startup composition. Every ownership and naming rule is
//!   checked once, before anything is served, so a running page cannot contain
//!   two owners of one key or an action that answers to two names.
//! - [`record`]: the flat record view of the document a capability reads, and
//!   the [`Delta`](record::Delta) between two views.
//! - [`plan`]: what a capability asks for (ops with batch-local references) and
//!   how a batch of plans from several capabilities is resolved into minted
//!   ids and checked ops, all or nothing.
//! - [`audit`]: the check the host runs on a browser replica's frame. It looks
//!   only at what the frame changed, so records the running composition does
//!   not understand are preserved rather than rejected for being present.
//! - [`capability`]: the trait a capability implements, and
//!   [`envelope`]: the JSON calling convention its WebAssembly exports use.
//!
//! Authorship is not part of any capability's vocabulary. The keys in
//! [`AUTHORSHIP_KEYS`] are written by the host from the authenticated caller,
//! and no descriptor, plan or replica frame may write them.

pub mod activity;
pub mod audit;
pub mod capability;
pub mod compose;
pub mod descriptor;
pub mod envelope;
pub mod plan;
pub mod record;
#[cfg(feature = "wasm-export")]
mod wasm;

pub use audit::{audit, AuditError, Violation};
pub use capability::{
    Call, CallerKind, CapError, Capability, Change, LegendEntry, Origin, Principal,
};
pub use compose::{ActionRef, ComposeError, Composition};
pub use descriptor::{
    ActionDecl, Artifact, Audience, Dep, Descriptor, Effect, KindClaim, PropClaim, Trigger,
};
pub use plan::{
    Batch, BatchBuilder, BatchEntry, Minter, Op, Plan, PlanError, ResolvedBatch, ResolvedOp,
    Target, Value,
};
pub use record::{Delta, Props, Record, RecordError, Scalar, View};

/// The contract version a descriptor, envelope and artifact manifest carry.
///
/// v2 adds [`Capability::plan_batch`] (and its export), the
/// [`Trigger::Property`] data trigger, and authority involvement by declared
/// reads. Every consumer, build step and verification recipe moves with it;
/// there is no dual-schema fallback.
pub const ABI: &str = "samepage.capability.v2";

/// The structural key. The kernel writes it from a `Create` op's `kind`; it is
/// never a property a capability declares or sets.
pub const KIND_KEY: &str = "kind";

/// Keys only the host writes, from the authenticated caller of an action or the
/// authenticated connection a replica frame arrived on.
pub const AUTHORSHIP_KEYS: &[&str] = &[
    "created_by",
    "touched_by",
    "created_by_participant",
    "touched_by_participant",
    "created_at",
];

/// Keys beginning with this prefix are reserved for the kernel.
pub const RESERVED_PREFIX: char = '_';

/// Whether `key` belongs to the kernel rather than to any capability.
pub fn is_reserved_key(key: &str) -> bool {
    key == KIND_KEY || AUTHORSHIP_KEYS.contains(&key) || key.starts_with(RESERVED_PREFIX)
}

#[cfg(feature = "wasm-export")]
#[doc(hidden)]
pub mod __private {
    pub use wasm_bindgen;
}
