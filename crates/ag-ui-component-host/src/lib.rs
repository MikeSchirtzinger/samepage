//! Wasmtime-backed host adapter for portable AG-UI extension components.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use ag_ui_component::{
    ActionDescriptor, ActionKind, CapabilityPolicy, CapabilityRequest, ComponentDescriptor,
    ExecutionLimits, ExecutionTarget, PolicyError,
};
use ag_ui_surface::{
    ActionDef, ClientModule, Effect, Extension, StateBacking, StateSnapshot, SurfaceState,
};
use bindings::exports::agui::component::types as wit;
use jsonschema::Validator;
use parking_lot::Mutex;
use serde_json::Value;
use thiserror::Error;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

const MAX_INTERNAL_CORE_INSTANCES: usize = 64;
const DENY_ALL_WASI_IMPORTS: &[&str] = &[
    "wasi:io/poll@0.2.6",
    "wasi:io/error@0.2.6",
    "wasi:io/streams@0.2.6",
    "wasi:cli/environment@0.2.6",
    "wasi:cli/exit@0.2.6",
    "wasi:cli/stdin@0.2.6",
    "wasi:cli/stdout@0.2.6",
    "wasi:cli/stderr@0.2.6",
    "wasi:cli/terminal-input@0.2.6",
    "wasi:cli/terminal-output@0.2.6",
    "wasi:cli/terminal-stdin@0.2.6",
    "wasi:cli/terminal-stdout@0.2.6",
    "wasi:cli/terminal-stderr@0.2.6",
    "wasi:clocks/wall-clock@0.2.6",
    "wasi:filesystem/types@0.2.6",
    "wasi:filesystem/preopens@0.2.6",
];

mod bindings {
    wasmtime::component::bindgen!({
        path: "../ag-ui-component/wit",
        world: "extension-component",
    });
}

/// A stateful WASI component projected as an existing `ag-ui-surface`
/// extension. Every action still enters the runtime through `ActionDef`.
pub struct HostedComponentExtension {
    descriptor: ComponentDescriptor,
    runtime: Arc<ComponentRuntime>,
    actions: Vec<ActionDef>,
}

impl HostedComponentExtension {
    /// Compile, instantiate, and verify a component against reviewed metadata
    /// and a deny-by-default policy.
    pub fn from_bytes(
        bytes: &[u8],
        expected: ComponentDescriptor,
        policy: CapabilityPolicy,
    ) -> Result<Self, HostError> {
        expected.validate()?;
        if policy.target != ExecutionTarget::Wasi {
            return Err(HostError::UnsupportedHostTarget(policy.target));
        }
        if let Some(grant) = policy.grants.first() {
            return Err(HostError::UnsupportedCapabilityGrant(grant.name.clone()));
        }
        let authorization = policy.authorize(&expected)?;
        if let Some(capability) = authorization.granted.first() {
            return Err(HostError::UnsupportedCapabilityGrant(capability.clone()));
        }

        let action_contracts = compile_action_contracts(&expected.actions)?;
        let runtime = Arc::new(ComponentRuntime::instantiate(
            bytes,
            policy.limits,
            action_contracts,
        )?);
        let actual = runtime.exported_descriptor()?;
        if actual != expected {
            return Err(HostError::DescriptorMismatch {
                expected: Box::new(expected),
                actual: Box::new(actual),
            });
        }

        let actions = build_surface_actions(&expected, Arc::clone(&runtime))?;
        Ok(Self {
            descriptor: expected,
            runtime,
            actions,
        })
    }

    pub fn descriptor(&self) -> &ComponentDescriptor {
        &self.descriptor
    }

    /// Opaque component-owned persistence payload.
    pub fn snapshot_bytes(&self) -> Result<Vec<u8>, HostError> {
        self.runtime.snapshot()
    }

    /// Restore a previously captured component-owned persistence payload.
    pub fn restore_bytes(&self, state: &[u8]) -> Result<(), HostError> {
        self.runtime.restore(state)
    }
}

impl Extension for HostedComponentExtension {
    fn id(&self) -> &str {
        &self.descriptor.id
    }

    fn version(&self) -> &str {
        &self.descriptor.version
    }

    fn state(&self) -> &dyn SurfaceState {
        self
    }

    fn actions(&self) -> &[ActionDef] {
        &self.actions
    }

    fn client_module(&self) -> Option<ClientModule> {
        None
    }

    fn capabilities(&self) -> Vec<&str> {
        self.descriptor
            .capabilities
            .iter()
            .map(|capability| capability.name.as_str())
            .collect()
    }
}

impl SurfaceState for HostedComponentExtension {
    fn backing(&self) -> StateBacking {
        StateBacking::LastWriterWins
    }

    fn describe(&self) -> Result<String, String> {
        self.runtime
            .describe_state()
            .map_err(|error| error.to_string())
    }

    fn snapshot(&self) -> Result<StateSnapshot, String> {
        let description = self.describe()?;
        let body = serde_json::from_str(&description)
            .map_err(|error| format!("component state description is not JSON: {error}"))?;
        Ok(StateSnapshot {
            backing: self.backing(),
            body,
            chrome: None,
        })
    }
}

#[derive(Debug, Error)]
pub enum HostError {
    #[error(transparent)]
    Policy(#[from] PolicyError),
    #[error("the Wasmtime adapter cannot host execution target {0:?}")]
    UnsupportedHostTarget(ExecutionTarget),
    #[error("the first Wasmtime adapter does not implement capability grant {0:?}")]
    UnsupportedCapabilityGrant(String),
    #[error("component descriptor mismatch: expected {expected:?}, component exported {actual:?}")]
    DescriptorMismatch {
        expected: Box<ComponentDescriptor>,
        actual: Box<ComponentDescriptor>,
    },
    #[error("invalid component action contract: {0}")]
    InvalidActionContract(String),
    #[error("failed to configure or instantiate component: {0}")]
    Instantiation(String),
    #[error("component imports unsupported host interface {0:?}")]
    UnsupportedImport(String),
    #[error("component operation {operation:?} failed: {message}")]
    Runtime {
        operation: &'static str,
        message: String,
    },
    #[error("component rejected action {action:?} with {code:?}: {message}")]
    Rejected {
        action: String,
        code: String,
        message: String,
    },
    #[error("component failed action {action:?} with {code:?}: {message}")]
    Failed {
        action: String,
        code: String,
        message: String,
    },
    #[error("component operation {operation:?} produced {actual} bytes; limit is {limit}")]
    OutputLimit {
        operation: &'static str,
        actual: usize,
        limit: u64,
    },
    #[error("component action {action:?} returned invalid output: {message}")]
    InvalidOutput { action: String, message: String },
    #[error("component query action {0:?} mutated component-owned state")]
    QueryMutatedState(String),
    #[error("component action {action:?} failed and rollback also failed; action: {cause}; rollback: {rollback}")]
    RollbackFailed {
        action: String,
        cause: String,
        rollback: String,
    },
    #[error("failed to start component deadline guard: {0}")]
    DeadlineGuard(String),
}

struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

struct InstanceState {
    store: Store<HostState>,
    bindings: bindings::ExtensionComponent,
}

struct ComponentRuntime {
    engine: Engine,
    component: Component,
    instance: Mutex<InstanceState>,
    limits: ExecutionLimits,
    contracts: HashMap<String, ActionContract>,
}

struct ActionContract {
    kind: ActionKind,
    output: Option<Arc<Validator>>,
}

impl ComponentRuntime {
    fn instantiate(
        bytes: &[u8],
        limits: ExecutionLimits,
        contracts: HashMap<String, ActionContract>,
    ) -> Result<Self, HostError> {
        limits.validate()?;
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine =
            Engine::new(&config).map_err(|error| HostError::Instantiation(error.to_string()))?;
        let component = Component::from_binary(&engine, bytes)
            .map_err(|error| HostError::Instantiation(error.to_string()))?;
        validate_component_imports(&component, &engine)?;
        let instance = instantiate_instance(&engine, &component, limits)?;

        Ok(Self {
            engine,
            component,
            instance: Mutex::new(instance),
            limits,
            contracts,
        })
    }

    fn exported_descriptor(&self) -> Result<ComponentDescriptor, HostError> {
        let descriptor = self.call("describe", |bindings, store| {
            bindings.agui_component_extension().call_describe(store)
        })?;
        let descriptor = descriptor.map_err(|message| HostError::Runtime {
            operation: "describe",
            message,
        })?;
        let descriptor = from_wit_descriptor(descriptor)?;
        descriptor.validate()?;
        let encoded = serde_json::to_vec(&descriptor).map_err(|error| HostError::Runtime {
            operation: "describe",
            message: format!("failed to measure exported descriptor: {error}"),
        })?;
        enforce_output_limit("describe", encoded.len(), self.limits.max_output_bytes)?;
        Ok(descriptor)
    }

    fn invoke(&self, action: &str, arguments: Value) -> Result<String, HostError> {
        let contract = self
            .contracts
            .get(action)
            .ok_or_else(|| HostError::Failed {
                action: action.to_string(),
                code: "undeclared-action".to_string(),
                message: "the action was not present in the reviewed descriptor".to_string(),
            })?;
        let request = wit::Invocation {
            action: action.to_string(),
            arguments_json: arguments.to_string(),
            context_json: "{}".to_string(),
        };
        let mut instance = self.instance.lock();
        let before = self.snapshot_locked(&mut instance)?;
        let outcome = self.call_locked(&mut instance, "invoke", |bindings, store| {
            bindings
                .agui_component_extension()
                .call_invoke(store, &request)
        });
        let result = match outcome {
            Err(error) => Err(error),
            Ok(wit::Outcome::Success(success)) => self.validate_success(action, contract, success),
            Ok(wit::Outcome::Rejected(error)) => Err(HostError::Rejected {
                action: action.to_string(),
                code: error.code,
                message: error.message,
            }),
            Ok(wit::Outcome::Failed(error)) => Err(HostError::Failed {
                action: action.to_string(),
                code: error.code,
                message: error.message,
            }),
        };

        let result = match result {
            Ok(result) => result,
            Err(cause) => {
                return match self.rollback_locked(&mut instance, &before) {
                    Ok(()) => Err(cause),
                    Err(rollback) => Err(HostError::RollbackFailed {
                        action: action.to_string(),
                        cause: cause.to_string(),
                        rollback: rollback.to_string(),
                    }),
                };
            }
        };

        if contract.kind == ActionKind::Query {
            let after = match self.snapshot_locked(&mut instance) {
                Ok(after) => after,
                Err(cause) => {
                    return match self.rollback_locked(&mut instance, &before) {
                        Ok(()) => Err(cause),
                        Err(rollback) => Err(HostError::RollbackFailed {
                            action: action.to_string(),
                            cause: cause.to_string(),
                            rollback: rollback.to_string(),
                        }),
                    };
                }
            };
            if after != before {
                self.rollback_locked(&mut instance, &before)
                    .map_err(|rollback| HostError::RollbackFailed {
                        action: action.to_string(),
                        cause: "query mutated component-owned state".to_string(),
                        rollback: rollback.to_string(),
                    })?;
                return Err(HostError::QueryMutatedState(action.to_string()));
            }
        }
        Ok(result)
    }

    fn validate_success(
        &self,
        action: &str,
        contract: &ActionContract,
        success: wit::Success,
    ) -> Result<String, HostError> {
        enforce_output_limit(
            "invoke",
            success
                .result_json
                .len()
                .saturating_add(success.events_json.len()),
            self.limits.max_output_bytes,
        )?;
        let events: Value = serde_json::from_str(&success.events_json).map_err(|error| {
            HostError::InvalidOutput {
                action: action.to_string(),
                message: format!("events-json is invalid JSON: {error}"),
            }
        })?;
        if events.as_array().is_none_or(|events| !events.is_empty()) {
            return Err(HostError::InvalidOutput {
                action: action.to_string(),
                message: "returned events are unsupported until a typed event adapter exists"
                    .to_string(),
            });
        }
        let result: Value = serde_json::from_str(&success.result_json).map_err(|error| {
            HostError::InvalidOutput {
                action: action.to_string(),
                message: format!("result-json is invalid JSON: {error}"),
            }
        })?;
        if let Some(validator) = &contract.output {
            let errors = validator
                .iter_errors(&result)
                .take(4)
                .map(|error| error.to_string())
                .collect::<Vec<_>>();
            if !errors.is_empty() {
                return Err(HostError::InvalidOutput {
                    action: action.to_string(),
                    message: errors.join("; "),
                });
            }
        }
        Ok(result.to_string())
    }

    fn describe_state(&self) -> Result<String, HostError> {
        let state = self.call("describe-state", |bindings, store| {
            bindings
                .agui_component_extension()
                .call_describe_state(store)
        })?;
        let state = state.map_err(|message| HostError::Runtime {
            operation: "describe-state",
            message,
        })?;
        enforce_output_limit("describe-state", state.len(), self.limits.max_output_bytes)?;
        serde_json::from_str::<Value>(&state).map_err(|error| HostError::Runtime {
            operation: "describe-state",
            message: format!("component returned invalid JSON: {error}"),
        })?;
        Ok(state)
    }

    fn snapshot(&self) -> Result<Vec<u8>, HostError> {
        let mut instance = self.instance.lock();
        self.snapshot_locked(&mut instance)
    }

    fn snapshot_locked(&self, instance: &mut InstanceState) -> Result<Vec<u8>, HostError> {
        let state = self.call_locked(instance, "snapshot", |bindings, store| {
            bindings.agui_component_extension().call_snapshot(store)
        })?;
        let state = state.map_err(|message| HostError::Runtime {
            operation: "snapshot",
            message,
        })?;
        enforce_output_limit("snapshot", state.len(), self.limits.max_output_bytes)?;
        Ok(state)
    }

    fn restore(&self, state: &[u8]) -> Result<(), HostError> {
        enforce_output_limit("restore", state.len(), self.limits.max_output_bytes)?;
        let mut instance = self.instance.lock();
        self.rollback_locked(&mut instance, state)
    }

    fn restore_locked(&self, instance: &mut InstanceState, state: &[u8]) -> Result<(), HostError> {
        let restored = self.call_locked(instance, "restore", |bindings, store| {
            bindings
                .agui_component_extension()
                .call_restore(store, state)
        })?;
        restored.map_err(|message| HostError::Runtime {
            operation: "restore",
            message,
        })
    }

    fn rollback_locked(&self, instance: &mut InstanceState, state: &[u8]) -> Result<(), HostError> {
        let first_error = match self.restore_locked(instance, state) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        let mut replacement = instantiate_instance(&self.engine, &self.component, self.limits)
            .map_err(|error| HostError::Runtime {
                operation: "rollback",
                message: format!(
                    "existing instance restore failed ({first_error}); recreation failed ({error})"
                ),
            })?;
        self.restore_locked(&mut replacement, state)
            .map_err(|error| HostError::Runtime {
                operation: "rollback",
                message: format!(
                    "existing instance restore failed ({first_error}); recreated instance restore failed ({error})"
                ),
            })?;
        *instance = replacement;
        Ok(())
    }

    fn call<T>(
        &self,
        operation: &'static str,
        call: impl FnOnce(&bindings::ExtensionComponent, &mut Store<HostState>) -> wasmtime::Result<T>,
    ) -> Result<T, HostError> {
        let mut instance = self.instance.lock();
        self.call_locked(&mut instance, operation, call)
    }

    fn call_locked<T>(
        &self,
        instance: &mut InstanceState,
        operation: &'static str,
        call: impl FnOnce(&bindings::ExtensionComponent, &mut Store<HostState>) -> wasmtime::Result<T>,
    ) -> Result<T, HostError> {
        prepare_store(&mut instance.store, self.limits)?;
        let deadline = DeadlineGuard::arm(self.engine.clone(), self.limits.timeout_ms)?;
        let InstanceState { store, bindings } = &mut *instance;
        let result = call(bindings, store).map_err(|error| HostError::Runtime {
            operation,
            message: error.to_string(),
        });
        deadline.cancel();
        result
    }
}

struct DeadlineGuard {
    cancel: mpsc::Sender<()>,
}

impl DeadlineGuard {
    fn arm(engine: Engine, timeout_ms: u64) -> Result<Self, HostError> {
        let (cancel, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("agui-component-deadline".to_string())
            .spawn(move || {
                if receiver
                    .recv_timeout(Duration::from_millis(timeout_ms))
                    .is_err()
                {
                    engine.increment_epoch();
                }
            })
            .map_err(|error| HostError::DeadlineGuard(error.to_string()))?;
        Ok(Self { cancel })
    }

    fn cancel(self) {
        let _ = self.cancel.send(());
    }
}

fn instantiate_instance(
    engine: &Engine,
    component: &Component,
    limits: ExecutionLimits,
) -> Result<InstanceState, HostError> {
    let memory_limit = usize::try_from(limits.max_memory_bytes).map_err(|_| {
        HostError::Instantiation("max_memory_bytes does not fit this host".to_string())
    })?;
    let mut linker = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
        .map_err(|error| HostError::Instantiation(error.to_string()))?;

    let mut wasi = WasiCtxBuilder::new();
    wasi.allow_tcp(false)
        .allow_udp(false)
        .allow_ip_name_lookup(false);
    let store_limits = StoreLimitsBuilder::new()
        .memory_size(memory_limit)
        // `ExecutionLimits::max_instances` counts hosted component
        // instances. This adapter owns exactly one. A Component Model
        // instance may internally instantiate several core modules and
        // adapters, so cap that separate implementation detail here.
        .instances(MAX_INTERNAL_CORE_INSTANCES)
        .memories(MAX_INTERNAL_CORE_INSTANCES)
        .tables(MAX_INTERNAL_CORE_INSTANCES)
        .trap_on_grow_failure(true)
        .build();
    let mut store = Store::new(
        engine,
        HostState {
            wasi: wasi.build(),
            table: ResourceTable::new(),
            limits: store_limits,
        },
    );
    store.limiter(|state| &mut state.limits);
    prepare_store(&mut store, limits)?;

    let deadline = DeadlineGuard::arm(engine.clone(), limits.timeout_ms)?;
    let bindings = bindings::ExtensionComponent::instantiate(&mut store, component, &linker)
        .map_err(|error| HostError::Instantiation(error.to_string()));
    deadline.cancel();
    Ok(InstanceState {
        store,
        bindings: bindings?,
    })
}

fn prepare_store(store: &mut Store<HostState>, limits: ExecutionLimits) -> Result<(), HostError> {
    store
        .set_fuel(limits.max_fuel)
        .map_err(|error| HostError::Instantiation(error.to_string()))?;
    store.epoch_deadline_trap();
    store.set_epoch_deadline(1);
    Ok(())
}

fn validate_component_imports(component: &Component, engine: &Engine) -> Result<(), HostError> {
    for (name, _) in component.component_type().imports(engine) {
        if !DENY_ALL_WASI_IMPORTS.contains(&name) {
            return Err(HostError::UnsupportedImport(name.to_string()));
        }
    }
    Ok(())
}

fn build_surface_actions(
    descriptor: &ComponentDescriptor,
    runtime: Arc<ComponentRuntime>,
) -> Result<Vec<ActionDef>, HostError> {
    descriptor
        .actions
        .iter()
        .map(|action| {
            let parameters = serde_json::from_str(&action.input_schema).map_err(|error| {
                HostError::InvalidActionContract(format!(
                    "action {:?} input schema is invalid JSON: {error}",
                    action.name
                ))
            })?;
            let name = action.name.clone();
            let invoke_name = name.clone();
            let invoke_runtime = Arc::clone(&runtime);
            let kind = action.kind;
            let apply = move |arguments: &Value| {
                let arguments = arguments.clone();
                let runtime = Arc::clone(&invoke_runtime);
                let action = invoke_name.clone();
                match kind {
                    ActionKind::Query => Effect::AsyncQuery(Box::pin(async move {
                        tokio::task::spawn_blocking(move || runtime.invoke(&action, arguments))
                            .await
                            .map_err(|error| format!("component host task failed: {error}"))?
                            .map_err(|error| error.to_string())
                    })),
                    ActionKind::Mutate => Effect::AsyncMutate(Box::pin(async move {
                        tokio::task::spawn_blocking(move || runtime.invoke(&action, arguments))
                            .await
                            .map_err(|error| format!("component host task failed: {error}"))?
                            .map(Some)
                            .map_err(|error| error.to_string())
                    })),
                }
            };
            Ok(ActionDef::new(
                name,
                action.description.clone(),
                parameters,
                apply,
            ))
        })
        .collect()
}

fn compile_action_contracts(
    actions: &[ActionDescriptor],
) -> Result<HashMap<String, ActionContract>, HostError> {
    let mut contracts = HashMap::with_capacity(actions.len());
    for action in actions {
        let input: Value = serde_json::from_str(&action.input_schema).map_err(|error| {
            HostError::InvalidActionContract(format!(
                "action {:?} input schema is invalid JSON: {error}",
                action.name
            ))
        })?;
        if !input.is_object() || input.get("type").and_then(Value::as_str) != Some("object") {
            return Err(HostError::InvalidActionContract(format!(
                "action {:?} input schema must be an object schema with type=object",
                action.name
            )));
        }
        compile_schema(&input).map_err(|message| {
            HostError::InvalidActionContract(format!(
                "action {:?} input schema is invalid: {message}",
                action.name
            ))
        })?;
        let output = action
            .output_schema
            .as_deref()
            .map(|schema| {
                let schema: Value = serde_json::from_str(schema).map_err(|error| {
                    HostError::InvalidActionContract(format!(
                        "action {:?} output schema is invalid JSON: {error}",
                        action.name
                    ))
                })?;
                compile_schema(&schema).map(Arc::new).map_err(|message| {
                    HostError::InvalidActionContract(format!(
                        "action {:?} output schema is invalid: {message}",
                        action.name
                    ))
                })
            })
            .transpose()?;
        if contracts
            .insert(
                action.name.clone(),
                ActionContract {
                    kind: action.kind,
                    output,
                },
            )
            .is_some()
        {
            return Err(HostError::InvalidActionContract(format!(
                "duplicate action {:?}",
                action.name
            )));
        }
    }
    Ok(contracts)
}

fn compile_schema(schema: &Value) -> Result<Validator, String> {
    let result = if schema.get("$schema").is_some() {
        jsonschema::validator_for(schema)
    } else {
        jsonschema::draft202012::options().build(schema)
    };
    result.map_err(|error| error.to_string())
}

fn from_wit_descriptor(value: wit::ComponentDescriptor) -> Result<ComponentDescriptor, HostError> {
    let capabilities = value
        .capabilities
        .into_iter()
        .map(from_wit_capability)
        .collect::<Result<Vec<_>, _>>()?;
    let actions = value
        .actions
        .into_iter()
        .map(|action| ActionDescriptor {
            name: action.name,
            description: action.description,
            kind: match action.kind {
                wit::ActionKind::Query => ActionKind::Query,
                wit::ActionKind::Mutate => ActionKind::Mutate,
            },
            input_schema: action.input_schema_json,
            output_schema: action.output_schema_json,
        })
        .collect();
    Ok(ComponentDescriptor {
        schema_version: value.schema_version,
        id: value.id,
        version: value.version,
        world: value.world_id,
        targets: value
            .targets
            .into_iter()
            .map(|target| match target {
                wit::ExecutionTarget::BrowserWasm => ExecutionTarget::BrowserWasm,
                wit::ExecutionTarget::Wasi => ExecutionTarget::Wasi,
                wit::ExecutionTarget::Native => ExecutionTarget::Native,
            })
            .collect(),
        capabilities,
        actions,
    })
}

fn from_wit_capability(value: wit::CapabilityRequest) -> Result<CapabilityRequest, HostError> {
    let mut scope = BTreeMap::new();
    for entry in value.scope {
        let value_count = entry.values.len();
        let values: BTreeSet<String> = entry.values.into_iter().collect();
        if values.is_empty()
            || values.len() != value_count
            || scope.insert(entry.key.clone(), values).is_some()
        {
            return Err(HostError::Policy(PolicyError::InvalidDescriptor(format!(
                "capability {:?} contains an empty or duplicate scope {:?}",
                value.name, entry.key
            ))));
        }
    }
    Ok(CapabilityRequest {
        name: value.name,
        required: value.required,
        scope,
    })
}

fn enforce_output_limit(
    operation: &'static str,
    actual: usize,
    limit: u64,
) -> Result<(), HostError> {
    if u64::try_from(actual).is_ok_and(|actual| actual <= limit) {
        Ok(())
    } else {
        Err(HostError::OutputLimit {
            operation,
            actual,
            limit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ag_ui_component::CapabilityGrant;

    #[test]
    fn invalid_output_schema_is_rejected_before_component_compilation() {
        let action = ActionDescriptor {
            name: "read".to_string(),
            description: "Read.".to_string(),
            kind: ActionKind::Query,
            input_schema: r#"{"type":"object"}"#.to_string(),
            output_schema: Some(r#"{"type":"not-a-json-schema-type"}"#.to_string()),
        };
        let error = compile_action_contracts(&[action])
            .err()
            .expect("an invalid schema must fail closed");
        assert!(matches!(error, HostError::InvalidActionContract(_)));
    }

    #[test]
    fn oversized_outputs_are_rejected() {
        assert!(matches!(
            enforce_output_limit("test", 11, 10),
            Err(HostError::OutputLimit { .. })
        ));
    }

    #[test]
    fn unimplemented_grants_fail_before_component_compilation() {
        let descriptor = ComponentDescriptor::new("probe", "0.1.0", vec![ExecutionTarget::Wasi]);
        let policy = CapabilityPolicy {
            target: ExecutionTarget::Wasi,
            grants: vec![CapabilityGrant::new("fs.read")],
            limits: ExecutionLimits::default(),
        };
        let error = HostedComponentExtension::from_bytes(&[], descriptor, policy)
            .err()
            .expect("unsupported grants must fail before invalid component bytes are compiled");
        assert!(matches!(error, HostError::UnsupportedCapabilityGrant(_)));
    }

    #[test]
    fn missing_required_capability_fails_before_component_compilation() {
        let mut descriptor =
            ComponentDescriptor::new("probe", "0.1.0", vec![ExecutionTarget::Wasi]);
        descriptor.capabilities = vec![CapabilityRequest::required("fs.read")];
        let policy = CapabilityPolicy {
            target: ExecutionTarget::Wasi,
            grants: Vec::new(),
            limits: ExecutionLimits::default(),
        };
        let error = HostedComponentExtension::from_bytes(&[], descriptor, policy)
            .err()
            .expect("required capabilities must fail before invalid component bytes are compiled");
        assert!(matches!(
            error,
            HostError::Policy(PolicyError::MissingRequiredCapability { .. })
        ));
    }

    #[test]
    fn ambient_network_and_random_imports_are_not_allowlisted() {
        assert!(!DENY_ALL_WASI_IMPORTS.contains(&"wasi:sockets/tcp@0.2.6"));
        assert!(!DENY_ALL_WASI_IMPORTS.contains(&"wasi:random/random@0.2.6"));
        assert!(DENY_ALL_WASI_IMPORTS.contains(&"wasi:cli/environment@0.2.6"));
    }
}
