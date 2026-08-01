//! Portable component metadata and deny-by-default capability policy.
//!
//! This crate intentionally contains no Wasmtime or browser bindings. It is the
//! policy/data layer both hosts can share before target-specific instantiation.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const COMPONENT_SCHEMA_VERSION: u32 = 1;
pub const EXTENSION_WORLD: &str = "agui:component/extension-component@0.1.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTarget {
    BrowserWasm,
    Wasi,
    Native,
}

/// A structured scope whose values must be a subset of the corresponding host
/// grant. For example `net.fetch` can request `domain = ["api.example.com"]`.
pub type CapabilityScope = BTreeMap<String, BTreeSet<String>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRequest {
    pub name: String,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub scope: CapabilityScope,
}

impl CapabilityRequest {
    pub fn required(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            required: true,
            scope: CapabilityScope::new(),
        }
    }

    pub fn optional(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            required: false,
            scope: CapabilityScope::new(),
        }
    }

    pub fn scoped(
        mut self,
        key: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.scope
            .insert(key.into(), values.into_iter().map(Into::into).collect());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityGrant {
    pub name: String,
    #[serde(default)]
    pub scope: CapabilityScope,
}

impl CapabilityGrant {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            scope: CapabilityScope::new(),
        }
    }

    pub fn scoped(
        mut self,
        key: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.scope
            .insert(key.into(), values.into_iter().map(Into::into).collect());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDescriptor {
    pub name: String,
    pub description: String,
    pub kind: ActionKind,
    pub input_schema: String,
    pub output_schema: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Query,
    Mutate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentDescriptor {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub world: String,
    pub targets: Vec<ExecutionTarget>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityRequest>,
    #[serde(default)]
    pub actions: Vec<ActionDescriptor>,
}

impl ComponentDescriptor {
    pub fn new(
        id: impl Into<String>,
        version: impl Into<String>,
        targets: Vec<ExecutionTarget>,
    ) -> Self {
        Self {
            schema_version: COMPONENT_SCHEMA_VERSION,
            id: id.into(),
            version: version.into(),
            world: EXTENSION_WORLD.to_string(),
            targets,
            capabilities: Vec::new(),
            actions: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.schema_version != COMPONENT_SCHEMA_VERSION {
            return Err(PolicyError::InvalidDescriptor(format!(
                "unsupported component schema_version {}; expected {COMPONENT_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        validate_token("component id", &self.id).map_err(PolicyError::InvalidDescriptor)?;
        if self.version.trim().is_empty() {
            return Err(PolicyError::InvalidDescriptor(
                "component version must not be empty".to_string(),
            ));
        }
        if self.world != EXTENSION_WORLD {
            return Err(PolicyError::InvalidDescriptor(format!(
                "component world {:?} does not match {EXTENSION_WORLD:?}",
                self.world
            )));
        }
        if self.targets.is_empty() {
            return Err(PolicyError::InvalidDescriptor(
                "component must support at least one execution target".to_string(),
            ));
        }
        let mut targets = HashSet::new();
        for target in &self.targets {
            if !targets.insert(*target) {
                return Err(PolicyError::InvalidDescriptor(format!(
                    "duplicate execution target {target:?}"
                )));
            }
        }

        let mut capabilities = HashSet::new();
        for capability in &self.capabilities {
            validate_token("capability name", &capability.name)
                .map_err(PolicyError::InvalidDescriptor)?;
            validate_scope(&capability.name, &capability.scope)
                .map_err(PolicyError::InvalidDescriptor)?;
            if !capabilities.insert(capability.name.as_str()) {
                return Err(PolicyError::InvalidDescriptor(format!(
                    "duplicate capability request {:?}",
                    capability.name
                )));
            }
        }

        let mut actions = HashSet::new();
        for action in &self.actions {
            validate_token("action name", &action.name).map_err(PolicyError::InvalidDescriptor)?;
            if action.description.trim().is_empty() {
                return Err(PolicyError::InvalidDescriptor(format!(
                    "action {:?} has an empty description",
                    action.name
                )));
            }
            if action.input_schema.trim().is_empty() {
                return Err(PolicyError::InvalidDescriptor(format!(
                    "action {:?} has an empty input schema",
                    action.name
                )));
            }
            if !actions.insert(action.name.as_str()) {
                return Err(PolicyError::InvalidDescriptor(format!(
                    "duplicate action {:?}",
                    action.name
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionLimits {
    pub max_memory_bytes: u64,
    pub max_fuel: u64,
    pub timeout_ms: u64,
    pub max_output_bytes: u64,
    pub max_instances: u32,
}

impl ExecutionLimits {
    pub fn validate(self) -> Result<(), PolicyError> {
        let values = [
            ("max_memory_bytes", self.max_memory_bytes),
            ("max_fuel", self.max_fuel),
            ("timeout_ms", self.timeout_ms),
            ("max_output_bytes", self.max_output_bytes),
            ("max_instances", u64::from(self.max_instances)),
        ];
        for (name, value) in values {
            if value == 0 {
                return Err(PolicyError::InvalidLimits(format!(
                    "{name} must be greater than zero"
                )));
            }
        }
        Ok(())
    }
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: 64 * 1024 * 1024,
            max_fuel: 10_000_000,
            timeout_ms: 5_000,
            max_output_bytes: 1024 * 1024,
            max_instances: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityPolicy {
    pub target: ExecutionTarget,
    #[serde(default)]
    pub grants: Vec<CapabilityGrant>,
    pub limits: ExecutionLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authorization {
    pub component_id: String,
    pub target: ExecutionTarget,
    pub granted: Vec<String>,
    pub unavailable_optional: Vec<String>,
    pub limits: ExecutionLimits,
}

impl CapabilityPolicy {
    /// Authorize a descriptor without adding implicit target capabilities.
    /// Required requests fail closed; unavailable optional requests are
    /// returned to the caller so degraded behavior is visible.
    pub fn authorize(
        &self,
        descriptor: &ComponentDescriptor,
    ) -> Result<Authorization, PolicyError> {
        descriptor.validate()?;
        self.limits.validate()?;
        if !descriptor.targets.contains(&self.target) {
            return Err(PolicyError::UnsupportedTarget {
                component: descriptor.id.clone(),
                target: self.target,
            });
        }

        let mut grant_names = HashSet::new();
        for grant in &self.grants {
            validate_token("grant name", &grant.name).map_err(PolicyError::InvalidGrant)?;
            validate_scope(&grant.name, &grant.scope).map_err(PolicyError::InvalidGrant)?;
            if !grant_names.insert(grant.name.as_str()) {
                return Err(PolicyError::InvalidGrant(format!(
                    "duplicate capability grant {:?}",
                    grant.name
                )));
            }
        }

        let mut granted = Vec::new();
        let mut unavailable_optional = Vec::new();
        for request in &descriptor.capabilities {
            let matching = self.grants.iter().find(|grant| grant.name == request.name);
            match matching {
                Some(grant) => {
                    ensure_scope_subset(&descriptor.id, request, grant)?;
                    granted.push(request.name.clone());
                }
                None if request.required => {
                    return Err(PolicyError::MissingRequiredCapability {
                        component: descriptor.id.clone(),
                        capability: request.name.clone(),
                    });
                }
                None => unavailable_optional.push(request.name.clone()),
            }
        }

        Ok(Authorization {
            component_id: descriptor.id.clone(),
            target: self.target,
            granted,
            unavailable_optional,
            limits: self.limits,
        })
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PolicyError {
    #[error("invalid component descriptor: {0}")]
    InvalidDescriptor(String),
    #[error("invalid capability grant: {0}")]
    InvalidGrant(String),
    #[error("invalid execution limits: {0}")]
    InvalidLimits(String),
    #[error("component {component:?} does not support target {target:?}")]
    UnsupportedTarget {
        component: String,
        target: ExecutionTarget,
    },
    #[error("component {component:?} requires missing capability {capability:?}")]
    MissingRequiredCapability {
        component: String,
        capability: String,
    },
    #[error(
        "component {component:?} requests {capability:?} scope {key:?} values {requested:?} outside grant {granted:?}"
    )]
    ScopeExceedsGrant {
        component: String,
        capability: String,
        key: String,
        requested: Vec<String>,
        granted: Vec<String>,
    },
}

fn default_true() -> bool {
    true
}

fn validate_token(label: &str, value: &str) -> Result<(), String> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        });
    if valid {
        Ok(())
    } else {
        Err(format!(
            "{label} {value:?} must be 1-128 lowercase ASCII letters, digits, '.', '_' or '-'"
        ))
    }
}

fn validate_scope(name: &str, scope: &CapabilityScope) -> Result<(), String> {
    for (key, values) in scope {
        validate_token("scope key", key)?;
        if values.is_empty() {
            return Err(format!(
                "capability {name:?} scope {key:?} must contain at least one value"
            ));
        }
        if values
            .iter()
            .any(|value| value.is_empty() || value.len() > 2048)
        {
            return Err(format!(
                "capability {name:?} scope {key:?} contains an empty or oversized value"
            ));
        }
    }
    Ok(())
}

fn ensure_scope_subset(
    component: &str,
    request: &CapabilityRequest,
    grant: &CapabilityGrant,
) -> Result<(), PolicyError> {
    if request.scope.is_empty() && !grant.scope.is_empty() {
        return Err(PolicyError::ScopeExceedsGrant {
            component: component.to_string(),
            capability: request.name.clone(),
            key: "<unscoped>".to_string(),
            requested: vec!["*".to_string()],
            granted: grant
                .scope
                .values()
                .flat_map(BTreeSet::iter)
                .cloned()
                .collect(),
        });
    }
    for (key, requested) in &request.scope {
        let granted = grant.scope.get(key);
        if granted.is_some_and(|values| requested.is_subset(values)) {
            continue;
        }
        return Err(PolicyError::ScopeExceedsGrant {
            component: component.to_string(),
            capability: request.name.clone(),
            key: key.clone(),
            requested: requested.iter().cloned().collect(),
            granted: granted
                .into_iter()
                .flat_map(BTreeSet::iter)
                .cloned()
                .collect(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> ComponentDescriptor {
        let mut descriptor = ComponentDescriptor::new(
            "notes",
            "0.1.0",
            vec![ExecutionTarget::BrowserWasm, ExecutionTarget::Wasi],
        );
        descriptor.capabilities = vec![
            CapabilityRequest::required("state.write").scoped("namespace", ["workspace.notes"]),
            CapabilityRequest::optional("net.fetch").scoped("domain", ["api.example.com"]),
        ];
        descriptor.actions.push(ActionDescriptor {
            name: "add-note".to_string(),
            description: "Add a durable note.".to_string(),
            kind: ActionKind::Mutate,
            input_schema: r#"{"type":"object"}"#.to_string(),
            output_schema: None,
        });
        descriptor
    }

    #[test]
    fn required_capabilities_fail_closed() {
        let error = CapabilityPolicy {
            target: ExecutionTarget::Wasi,
            grants: Vec::new(),
            limits: ExecutionLimits::default(),
        }
        .authorize(&descriptor())
        .expect_err("a required state grant must not be implicit");

        assert!(matches!(
            error,
            PolicyError::MissingRequiredCapability { capability, .. }
                if capability == "state.write"
        ));
    }

    #[test]
    fn optional_capabilities_are_visible_when_unavailable() {
        let authorization = CapabilityPolicy {
            target: ExecutionTarget::BrowserWasm,
            grants: vec![
                CapabilityGrant::new("state.write").scoped("namespace", ["workspace.notes"])
            ],
            limits: ExecutionLimits::default(),
        }
        .authorize(&descriptor())
        .expect("the required grant is exact");

        assert_eq!(authorization.granted, ["state.write"]);
        assert_eq!(authorization.unavailable_optional, ["net.fetch"]);
    }

    #[test]
    fn requested_scope_must_be_within_grant() {
        let error = CapabilityPolicy {
            target: ExecutionTarget::Wasi,
            grants: vec![
                CapabilityGrant::new("state.write").scoped("namespace", ["workspace.other"])
            ],
            limits: ExecutionLimits::default(),
        }
        .authorize(&descriptor())
        .expect_err("a different namespace must be denied");

        assert!(matches!(error, PolicyError::ScopeExceedsGrant { .. }));
    }

    #[test]
    fn unsupported_target_is_rejected_before_instantiation() {
        let error = CapabilityPolicy {
            target: ExecutionTarget::Native,
            grants: Vec::new(),
            limits: ExecutionLimits::default(),
        }
        .authorize(&descriptor())
        .expect_err("the descriptor does not advertise native support");

        assert!(matches!(error, PolicyError::UnsupportedTarget { .. }));
    }

    #[test]
    fn an_unscoped_request_requires_an_unscoped_grant() {
        let mut descriptor =
            ComponentDescriptor::new("network-probe", "0.1.0", vec![ExecutionTarget::Wasi]);
        descriptor.capabilities = vec![CapabilityRequest::required("net.fetch")];
        let error = CapabilityPolicy {
            target: ExecutionTarget::Wasi,
            grants: vec![CapabilityGrant::new("net.fetch").scoped("domain", ["api.example.com"])],
            limits: ExecutionLimits::default(),
        }
        .authorize(&descriptor)
        .expect_err("a scoped grant must not satisfy an unrestricted request");

        assert!(matches!(error, PolicyError::ScopeExceedsGrant { .. }));
    }

    #[test]
    fn malformed_host_grants_are_reported_as_host_policy_errors() {
        let error = CapabilityPolicy {
            target: ExecutionTarget::Wasi,
            grants: vec![CapabilityGrant::new("State.Write")],
            limits: ExecutionLimits::default(),
        }
        .authorize(&descriptor())
        .expect_err("grant names must use the portable token format");

        assert!(matches!(error, PolicyError::InvalidGrant(_)));
    }

    #[test]
    fn descriptor_round_trips_without_losing_policy_data() {
        let source = descriptor();
        let encoded = serde_json::to_string(&source).expect("descriptor serializes");
        let decoded: ComponentDescriptor =
            serde_json::from_str(&encoded).expect("descriptor deserializes");
        assert_eq!(decoded, source);
    }
}
