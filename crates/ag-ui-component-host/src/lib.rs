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
/// The interfaces the `wasm32-wasip2` adapter links into every component
/// whether or not the guest ever calls them, and which this host answers with a
/// context that grants nothing: no preopened directory, no environment, no
/// inherited stdio.
///
/// **What passing this check does and does not prove.** It proves the artifact
/// cannot reach an interface outside this list — no sockets, no outgoing HTTP,
/// no `wasi:random`. It does **not** prove the component is inert, and it is not
/// the `declared capabilities ⊇ actual imports` rule: nothing here is compared
/// against the descriptor at all. The counter example declares
/// `capabilities: []` and still imports filesystem and environment from this
/// list, so a descriptor claiming zero capabilities remains an unverified claim
/// about *these* interfaces. It is only checked against the ones that would
/// grant ambient authority the host context cannot take back.
///
/// Closing that gap properly means building against a world that pulls no
/// adapter at all, so a component with zero imports is self-evidently inert
/// rather than trusted-by-host-configuration. Mapping each interface to a
/// product-level capability name instead would need a table somebody maintains
/// by hand, which fails closed on legitimate components when it goes stale and
/// fails open on interfaces added after it was written.
///
/// Versions are deliberately absent: the interface identity is the part that
/// carries authority, and pinning `@0.2.6` here meant a toolchain bump refused
/// every component at once — which gets fixed by pasting in names, exactly the
/// reflex this list exists to prevent.
const INERT_WASI_INTERFACES: &[&str] = &[
    "wasi:io/poll",
    "wasi:io/error",
    "wasi:io/streams",
    "wasi:cli/environment",
    "wasi:cli/exit",
    "wasi:cli/stdin",
    "wasi:cli/stdout",
    "wasi:cli/stderr",
    "wasi:cli/terminal-input",
    "wasi:cli/terminal-output",
    "wasi:cli/terminal-stdin",
    "wasi:cli/terminal-stdout",
    "wasi:cli/terminal-stderr",
    "wasi:clocks/wall-clock",
    "wasi:filesystem/types",
    "wasi:filesystem/preopens",
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
        if !import_is_inert(name) {
            return Err(HostError::UnsupportedImport(name.to_string()));
        }
    }
    Ok(())
}

/// Whether one import names an interface from [`INERT_WASI_INTERFACES`],
/// ignoring its version.
///
/// Split out from the loop so it can be tested against real import names
/// without compiling a component for every case — and so the version-stripping
/// rule is stated once rather than implied by a string comparison.
fn import_is_inert(name: &str) -> bool {
    let interface = name.split_once('@').map_or(name, |(head, _)| head);
    INERT_WASI_INTERFACES.contains(&interface)
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
    use std::path::PathBuf;

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
        // The interfaces that would hand a component authority the host context
        // cannot take back once it is linked.
        for reachable in [
            "wasi:sockets/tcp@0.2.6",
            "wasi:sockets/udp@0.2.6",
            "wasi:sockets/ip-name-lookup@0.2.6",
            "wasi:random/random@0.2.6",
            "wasi:random/insecure-seed@0.2.6",
            "wasi:http/outgoing-handler@0.2.6",
            "wasi:clocks/monotonic-clock@0.2.6",
        ] {
            assert!(
                !import_is_inert(reachable),
                "{reachable} must not load: nothing downstream re-checks it"
            );
        }
        assert!(import_is_inert("wasi:cli/environment@0.2.6"));
    }

    /// A version bump must not turn the check into "refuse everything", because
    /// the fix for that is pasting names into the list without reading them.
    #[test]
    fn an_interface_is_recognised_across_wasi_versions() {
        for version in ["@0.2.0", "@0.2.6", "@0.2.9", "@0.3.0", ""] {
            assert!(
                import_is_inert(&format!("wasi:io/streams{version}")),
                "wasi:io/streams{version} is the same interface at every version"
            );
            assert!(
                !import_is_inert(&format!("wasi:sockets/tcp{version}")),
                "and sockets is refused at every version too"
            );
        }
    }

    /// A near-miss must not pass. Prefix matching on `wasi:cli/std` would admit
    /// anything that merely starts like an allowed name.
    #[test]
    fn a_name_that_only_resembles_an_allowed_interface_is_refused() {
        for impostor in [
            "wasi:cli/stdout-evil@0.2.6",
            "wasi:filesystem/types-extra",
            "evil:io/streams@0.2.6",
            "wasi:io/streams-plus@0.2.6",
        ] {
            assert!(!import_is_inert(impostor), "{impostor} must not load");
        }
    }

    /// The check against a real artifact rather than against its own list.
    ///
    /// Skipped rather than failed when the component has not been built: the
    /// `wasm32-wasip2` target is not present on every machine, and a test that
    /// fails on a missing build artifact teaches people to ignore it.
    #[test]
    fn the_built_counter_component_loads_and_imports_nothing_outside_the_list() {
        let Some(bytes) = counter_component_bytes() else {
            eprintln!("skipped: component_counter_extension.wasm has not been built");
            return;
        };
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config).expect("engine");
        let component = Component::from_binary(&engine, &bytes).expect("the component compiles");

        let imports: Vec<String> = component
            .component_type()
            .imports(&engine)
            .map(|(name, _)| name.to_string())
            .collect();
        assert!(
            !imports.is_empty(),
            "a component with no imports at all would make this test vacuous — the day that \
             is true, this check has been replaced by something stronger"
        );
        let refused: Vec<&String> = imports
            .iter()
            .filter(|name| !import_is_inert(name))
            .collect();
        assert!(
            refused.is_empty(),
            "the shipped component must still load; these imports were refused: {refused:?}"
        );
        validate_component_imports(&component, &engine)
            .expect("and the whole check must agree with the per-import rule");
    }

    /// What this check is *not*. Written as a test so the limit is discovered by
    /// reading the suite rather than by trusting a comment: the component that
    /// motivated this work declares no capabilities and still imports filesystem
    /// and environment, and it loads.
    #[test]
    fn a_descriptor_claiming_no_capabilities_is_still_an_unverified_claim() {
        let Some(bytes) = counter_component_bytes() else {
            eprintln!("skipped: component_counter_extension.wasm has not been built");
            return;
        };
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config).expect("engine");
        let component = Component::from_binary(&engine, &bytes).expect("the component compiles");
        let imports: Vec<String> = component
            .component_type()
            .imports(&engine)
            .map(|(name, _)| name.to_string())
            .collect();

        let descriptor: ComponentDescriptor = serde_json::from_str(include_str!(
            "../../../examples/component-counter-extension/descriptor.json"
        ))
        .expect("the shipped descriptor parses");

        assert!(
            descriptor.capabilities.is_empty(),
            "this test is about a descriptor that declares nothing"
        );
        assert!(
            imports.iter().any(|name| name.starts_with("wasi:filesystem/")
                || name.starts_with("wasi:cli/environment")),
            "and about it importing interfaces anyway: {imports:?}"
        );
        assert!(
            validate_component_imports(&component, &engine).is_ok(),
            "which loads today. `declared ⊇ actual` would refuse it, and the honest fix is a \
             world that pulls no adapter — not a hand-maintained interface-to-capability table"
        );
    }

    fn counter_component_bytes() -> Option<Vec<u8>> {
        // Cargo does not tell a host crate where a wasm32 artifact landed, and
        // the target directory is relocatable, so both the workspace default and
        // an overridden CARGO_TARGET_DIR are checked.
        let mut roots = Vec::new();
        if let Ok(target) = std::env::var("CARGO_TARGET_DIR") {
            roots.push(PathBuf::from(target));
        }
        roots.push(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()?
                .parent()?
                .join("target"),
        );
        for root in roots {
            for profile in ["debug", "release"] {
                let path = root
                    .join("wasm32-wasip2")
                    .join(profile)
                    .join("component_counter_extension.wasm");
                if let Ok(bytes) = std::fs::read(&path) {
                    return Some(bytes);
                }
            }
        }
        None
    }
}
