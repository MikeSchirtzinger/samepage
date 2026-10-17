//! Startup composition of a capability set.
//!
//! [`Composition::new`] checks every rule that makes ownership of the shared
//! document unambiguous and every naming rule for actions, and refuses the
//! whole set on the first violation. A composition that exists is therefore
//! one where each kind has one owner, each key on each kind has one writer,
//! and each action name, preferred or alias, resolves to one action.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::descriptor::{Artifact, Descriptor, Trigger};
use crate::{is_reserved_key, ABI};

/// Preferred action names may not carry these product prefixes. They survive
/// only as aliases.
const PRODUCT_PREFIXES: &[&str] = &["atlas_", "room_"];

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ComposeError {
    #[error("capability {capability:?} declares ABI {found:?}; this host speaks {ABI}")]
    AbiMismatch { capability: String, found: String },
    #[error("capability id {0:?} is not lowercase words joined by '-'")]
    InvalidId(String),
    #[error("capability {0:?} has an empty version")]
    EmptyVersion(String),
    #[error("capability {0:?} is declared twice")]
    DuplicateCapability(String),
    #[error(
        "capability {capability:?} names kind {kind:?}, which is not lowercase words joined by '-'"
    )]
    InvalidKind { capability: String, kind: String },
    #[error("kind {kind:?} is owned by both {first:?} and {second:?}")]
    DuplicateKindOwner {
        kind: String,
        first: String,
        second: String,
    },
    #[error("capability {capability:?} claims {key:?} on {kind:?}, which only the kernel writes")]
    ReservedKey {
        capability: String,
        kind: String,
        key: String,
    },
    #[error("capability {capability:?} claims {key:?} on {kind:?}, which is not a lowercase snake_case key")]
    InvalidKey {
        capability: String,
        kind: String,
        key: String,
    },
    #[error("key {key:?} on {kind:?} is claimed by both {first:?} and {second:?}")]
    DuplicateKey {
        kind: String,
        key: String,
        first: String,
        second: String,
    },
    #[error("capability {capability:?} extends kind {kind:?}, which no earlier capability owns")]
    PropClaimWithoutOwner { capability: String, kind: String },
    #[error(
        "capability {capability:?} extends its own kind {kind:?}; list the keys in its kind claim"
    )]
    PropClaimOnOwnKind { capability: String, kind: String },
    #[error("capability {capability:?} extends {kind:?}, owned by {owner:?}, without depending on {owner:?}")]
    PropClaimWithoutDependency {
        capability: String,
        kind: String,
        owner: String,
    },
    #[error("capability {capability:?} reads {kind:?}, which none of its dependencies owns")]
    ReadWithoutDependency { capability: String, kind: String },
    #[error(
        "capability {capability:?} depends on {dependency:?}, which is not in this composition"
    )]
    MissingDependency {
        capability: String,
        dependency: String,
    },
    #[error("capability {capability:?} depends on {dependency:?}, which is declared after it")]
    DependencyOrder {
        capability: String,
        dependency: String,
    },
    #[error("capability {capability:?} needs {dependency:?} {wanted}, found {found}")]
    DependencyVersion {
        capability: String,
        dependency: String,
        wanted: String,
        found: String,
    },
    #[error("capability {0:?} depends on itself")]
    SelfDependency(String),
    #[error("capability {capability:?} declares action name {name:?}, which is not lowercase noun_verb words")]
    InvalidActionName { capability: String, name: String },
    #[error("capability {capability:?} uses {name:?} as a public name; product-prefixed names may only be aliases")]
    ProductPrefix { capability: String, name: String },
    #[error("action name {name:?} is declared by both {first:?} and {second:?}")]
    DuplicateActionName {
        name: String,
        first: String,
        second: String,
    },
    #[error("capability {0:?} claims part of the document but declares a native-only artifact; its projection and validation must ship as WebAssembly")]
    NativeOnlyClaimsDocument(String),
    #[error("capability {0:?} declares a native-only artifact without a reason")]
    NativeOnlyWithoutReason(String),
    #[error("capability {capability:?} triggers on kind {kind:?}, which it neither owns, extends nor reads")]
    TriggerKindNotVisible { capability: String, kind: String },
    #[error(
        "capability {capability:?} triggers on {kind:?}.{key}, a key it neither owns nor extends"
    )]
    TriggerKeyNotOwned {
        capability: String,
        kind: String,
        key: String,
    },
}

/// How an action name resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRef<'a> {
    pub capability: &'a str,
    pub preferred: &'a str,
    /// True when the caller used a legacy alias.
    pub alias: bool,
}

/// A checked capability set, in dependency order.
#[derive(Debug, Clone)]
pub struct Composition {
    descriptors: Vec<Descriptor>,
    kind_owner: BTreeMap<String, String>,
    key_writer: BTreeMap<(String, String), String>,
    /// name (preferred or alias) -> (capability, preferred, is_alias)
    action_names: BTreeMap<String, (String, String, bool)>,
    /// capability -> the capabilities it names in `depends_on`. Directly
    /// declared only: a planner's reach is what its descriptor says out loud,
    /// not what its dependencies happen to depend on.
    direct: BTreeMap<String, BTreeSet<String>>,
    /// kind -> every capability that owns it, extends it, or declared it in
    /// `reads`. These are the capabilities with something to say about a
    /// record of that kind.
    kind_interest: BTreeMap<String, BTreeSet<String>>,
}

impl Composition {
    /// Check and compose. Descriptors must be listed in dependency order: a
    /// capability's dependencies appear before it, which also rules out
    /// cycles.
    pub fn new(descriptors: Vec<Descriptor>) -> Result<Self, ComposeError> {
        let mut composition = Composition {
            descriptors: Vec::new(),
            kind_owner: BTreeMap::new(),
            key_writer: BTreeMap::new(),
            action_names: BTreeMap::new(),
            direct: BTreeMap::new(),
            kind_interest: BTreeMap::new(),
        };
        let all_ids: BTreeSet<String> = descriptors.iter().map(|d| d.id.clone()).collect();
        // Transitive dependencies of each capability already accepted.
        let mut closure: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        for descriptor in &descriptors {
            let id = descriptor.id.clone();
            if descriptor.abi != ABI {
                return Err(ComposeError::AbiMismatch {
                    capability: id,
                    found: descriptor.abi.clone(),
                });
            }
            if !is_dashed_words(&id) {
                return Err(ComposeError::InvalidId(id));
            }
            if descriptor.version.is_empty() {
                return Err(ComposeError::EmptyVersion(id));
            }
            if closure.contains_key(&id) {
                return Err(ComposeError::DuplicateCapability(id));
            }

            let mut reach = BTreeSet::new();
            for dep in &descriptor.depends_on {
                if dep.id == id {
                    return Err(ComposeError::SelfDependency(id));
                }
                let Some(found) = composition.descriptor(&dep.id) else {
                    return Err(if all_ids.contains(&dep.id) {
                        ComposeError::DependencyOrder {
                            capability: id,
                            dependency: dep.id.clone(),
                        }
                    } else {
                        ComposeError::MissingDependency {
                            capability: id,
                            dependency: dep.id.clone(),
                        }
                    });
                };
                if found.version != dep.version {
                    return Err(ComposeError::DependencyVersion {
                        capability: id,
                        dependency: dep.id.clone(),
                        wanted: dep.version.clone(),
                        found: found.version.clone(),
                    });
                }
                reach.insert(dep.id.clone());
                reach.extend(closure[&dep.id].iter().cloned());
            }

            if let Artifact::NativeOnly { reason } = &descriptor.artifact {
                if reason.trim().is_empty() {
                    return Err(ComposeError::NativeOnlyWithoutReason(id));
                }
                if descriptor.claims_document() {
                    return Err(ComposeError::NativeOnlyClaimsDocument(id));
                }
            }

            for claim in &descriptor.kinds {
                if !is_dashed_words(&claim.kind) {
                    return Err(ComposeError::InvalidKind {
                        capability: id,
                        kind: claim.kind.clone(),
                    });
                }
                if let Some(first) = composition.kind_owner.get(&claim.kind) {
                    return Err(ComposeError::DuplicateKindOwner {
                        kind: claim.kind.clone(),
                        first: first.clone(),
                        second: id,
                    });
                }
                composition
                    .kind_owner
                    .insert(claim.kind.clone(), id.clone());
                composition.claim_keys(&id, &claim.kind, &claim.keys)?;
            }

            for claim in &descriptor.props {
                let Some(owner) = composition.kind_owner.get(&claim.kind).cloned() else {
                    return Err(ComposeError::PropClaimWithoutOwner {
                        capability: id,
                        kind: claim.kind.clone(),
                    });
                };
                if owner == id {
                    return Err(ComposeError::PropClaimOnOwnKind {
                        capability: id,
                        kind: claim.kind.clone(),
                    });
                }
                if !reach.contains(&owner) {
                    return Err(ComposeError::PropClaimWithoutDependency {
                        capability: id,
                        kind: claim.kind.clone(),
                        owner,
                    });
                }
                composition.claim_keys(&id, &claim.kind, &claim.keys)?;
            }

            for kind in &descriptor.reads {
                let owned_by_dependency = composition
                    .kind_owner
                    .get(kind)
                    .is_some_and(|owner| reach.contains(owner));
                if !owned_by_dependency {
                    return Err(ComposeError::ReadWithoutDependency {
                        capability: id,
                        kind: kind.clone(),
                    });
                }
            }

            for action in &descriptor.actions {
                if !is_snake_words(&action.name) {
                    return Err(ComposeError::InvalidActionName {
                        capability: id,
                        name: action.name.clone(),
                    });
                }
                if PRODUCT_PREFIXES
                    .iter()
                    .any(|prefix| action.name.starts_with(prefix))
                {
                    return Err(ComposeError::ProductPrefix {
                        capability: id,
                        name: action.name.clone(),
                    });
                }
                composition.name_action(&id, &action.name, &action.name, false)?;
                for alias in &action.aliases {
                    if !is_snake_words(alias) {
                        return Err(ComposeError::InvalidActionName {
                            capability: id,
                            name: alias.clone(),
                        });
                    }
                    composition.name_action(&id, alias, &action.name, true)?;
                }
            }

            // A data trigger names data this capability can actually see, and
            // a property trigger names a key it owns or extends.
            for trigger in &descriptor.triggers {
                let visible = |kind: &str| {
                    descriptor.kinds.iter().any(|claim| claim.kind == kind)
                        || descriptor.props.iter().any(|claim| claim.kind == kind)
                        || descriptor.reads.iter().any(|read| read == kind)
                };
                match trigger {
                    Trigger::Always => {}
                    Trigger::Kind { name } => {
                        if !visible(name) {
                            return Err(ComposeError::TriggerKindNotVisible {
                                capability: id,
                                kind: name.clone(),
                            });
                        }
                    }
                    // Tool and route triggers keep the semantics they have
                    // always had. A tool name is the host's tool selection,
                    // which need not be one of this capability's actions, and
                    // a route is whatever the host routes. This amendment adds
                    // the property trigger; it does not introduce tool or
                    // route policy.
                    Trigger::Tool { .. } | Trigger::Route { .. } => {}
                    Trigger::Property { kind, key } => {
                        if !visible(kind) {
                            return Err(ComposeError::TriggerKindNotVisible {
                                capability: id,
                                kind: kind.clone(),
                            });
                        }
                        let owns_key = descriptor
                            .kinds
                            .iter()
                            .map(|claim| (&claim.kind, &claim.keys))
                            .chain(
                                descriptor
                                    .props
                                    .iter()
                                    .map(|claim| (&claim.kind, &claim.keys)),
                            )
                            .any(|(claimed, keys)| claimed == kind && keys.contains(key));
                        if !owns_key {
                            return Err(ComposeError::TriggerKeyNotOwned {
                                capability: id,
                                kind: kind.clone(),
                                key: key.clone(),
                            });
                        }
                    }
                }
            }

            for kind in descriptor
                .kinds
                .iter()
                .map(|claim| &claim.kind)
                .chain(descriptor.props.iter().map(|claim| &claim.kind))
                .chain(descriptor.reads.iter())
            {
                composition
                    .kind_interest
                    .entry(kind.clone())
                    .or_default()
                    .insert(id.clone());
            }

            closure.insert(id.clone(), reach);
            composition.direct.insert(
                id.clone(),
                descriptor
                    .depends_on
                    .iter()
                    .map(|dep| dep.id.clone())
                    .collect(),
            );
            composition.descriptors.push(descriptor.clone());
        }
        Ok(composition)
    }

    fn claim_keys(
        &mut self,
        capability: &str,
        kind: &str,
        keys: &[String],
    ) -> Result<(), ComposeError> {
        for key in keys {
            if is_reserved_key(key) {
                return Err(ComposeError::ReservedKey {
                    capability: capability.to_string(),
                    kind: kind.to_string(),
                    key: key.clone(),
                });
            }
            if !is_snake_key(key) {
                return Err(ComposeError::InvalidKey {
                    capability: capability.to_string(),
                    kind: kind.to_string(),
                    key: key.clone(),
                });
            }
            let slot = (kind.to_string(), key.clone());
            if let Some(first) = self.key_writer.get(&slot) {
                return Err(ComposeError::DuplicateKey {
                    kind: kind.to_string(),
                    key: key.clone(),
                    first: first.clone(),
                    second: capability.to_string(),
                });
            }
            self.key_writer.insert(slot, capability.to_string());
        }
        Ok(())
    }

    fn name_action(
        &mut self,
        capability: &str,
        name: &str,
        preferred: &str,
        alias: bool,
    ) -> Result<(), ComposeError> {
        if let Some((first, _, _)) = self.action_names.get(name) {
            return Err(ComposeError::DuplicateActionName {
                name: name.to_string(),
                first: first.clone(),
                second: capability.to_string(),
            });
        }
        self.action_names.insert(
            name.to_string(),
            (capability.to_string(), preferred.to_string(), alias),
        );
        Ok(())
    }

    /// Every capability that owns, extends or reads `kind`.
    ///
    /// Reading is not writing: a reader is consulted about a change to that
    /// kind and may refuse it, but it can never write a key it does not own.
    pub fn interested_in_kind(&self, kind: &str) -> impl Iterator<Item = &str> + use<'_> {
        self.kind_interest
            .get(kind)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }

    /// Whether `planner` may put an entry for `target` in a planned batch:
    /// itself, or a capability it names in `depends_on` directly. A dependency
    /// of a dependency does not count, so the descriptor of a capability that
    /// writes another's records always says whose records they are.
    pub fn may_target(&self, planner: &str, target: &str) -> bool {
        planner == target
            || self
                .direct
                .get(planner)
                .is_some_and(|direct| direct.contains(target))
    }

    pub fn descriptors(&self) -> &[Descriptor] {
        &self.descriptors
    }

    pub fn descriptor(&self, id: &str) -> Option<&Descriptor> {
        self.descriptors.iter().find(|d| d.id == id)
    }

    /// The capability that owns `kind`, if any enabled capability does.
    pub fn owner_of_kind(&self, kind: &str) -> Option<&str> {
        self.kind_owner.get(kind).map(String::as_str)
    }

    /// The one capability allowed to write `key` on records of `kind`.
    pub fn writer_of(&self, kind: &str, key: &str) -> Option<&str> {
        self.key_writer
            .get(&(kind.to_string(), key.to_string()))
            .map(String::as_str)
    }

    pub fn may_write(&self, capability: &str, kind: &str, key: &str) -> bool {
        self.writer_of(kind, key) == Some(capability)
    }

    /// Resolve a preferred name or a legacy alias.
    pub fn resolve_action(&self, name: &str) -> Option<ActionRef<'_>> {
        self.action_names
            .get(name)
            .map(|(capability, preferred, alias)| ActionRef {
                capability,
                preferred,
                alias: *alias,
            })
    }

    /// Public action names only: what a tool list or a document may show.
    pub fn public_action_names(&self) -> Vec<&str> {
        self.action_names
            .iter()
            .filter(|(_, (_, _, alias))| !alias)
            .map(|(name, _)| name.as_str())
            .collect()
    }
}

/// `map`, `understanding-context`, `explanation-flow`.
fn is_dashed_words(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|word| {
            word.starts_with(|c: char| c.is_ascii_lowercase())
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// `card_place`, `page_read`.
fn is_snake_words(value: &str) -> bool {
    !value.is_empty()
        && value
            .split('_')
            .all(|word| !word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase()))
}

/// `w`, `h_pref`, `segment_parts`, `state_initial`.
fn is_snake_key(value: &str) -> bool {
    value.starts_with(|c: char| c.is_ascii_lowercase())
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}
