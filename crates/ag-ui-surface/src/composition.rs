//! Compile-time server extension composition and the pinned app recipe.
//!
//! A recipe never downloads or executes code. Applications compile a concrete
//! set of [`Extension`] implementations, then [`CompositeSurface::from_recipe`]
//! selects and validates the locally available implementations. The result is
//! an ordinary [`Surface`], so every action still flows through the runtime's
//! one dispatcher and every route is mounted by the same host.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Map as JsonMap, Value as JsonValue};
use thiserror::Error;

use crate::services::{
    DocumentClaim, DocumentRegistry, ExtensionDependency, ServiceRegistry, ServiceRequirement,
};
use crate::{
    ActionRouteDef, ClientModule, HttpMethod, RouteDef, StateBacking, StateSnapshot, Surface,
    SurfaceState, SurfaceStore, ToolDef,
};
use crate::{SemanticTarget, SemanticTargetRef};

/// A strict, reviewable `agui.app.toml` failure. Recipe errors are startup
/// errors by design: silently dropping an unavailable extension would make the
/// runtime manifest claim a different application than the user approved.
#[derive(Debug, Error)]
pub enum CompositionError {
    #[error("could not read app recipe {path}: {source}")]
    RecipeRead {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not parse app recipe {path}: {source}")]
    RecipeParse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid app recipe: {0}")]
    InvalidRecipe(String),
    #[error("invalid compiled extension set: {0}")]
    InvalidExtensions(String),
    /// A declared host service had no provider, or had one of the wrong type.
    /// Separate from [`Self::InvalidExtensions`] because the fix is in the
    /// application's startup wiring, not in the extension set or the recipe.
    #[error(
        "extension {extension} requires host service {service:?} ({reason}), \
         which this application does not provide (available: {available:?})"
    )]
    MissingService {
        extension: String,
        service: String,
        reason: String,
        available: Vec<String>,
    },
    /// A declared extension dependency did not resolve to an enabled extension
    /// at the expected version, was circular, or was ordered after its
    /// dependent.
    #[error("unsatisfied extension dependency: {0}")]
    UnsatisfiedDependency(String),
}

/// The install/runtime selection recipe. This is intentionally smaller than a
/// marketplace package manifest: it pins the locally compiled extension ids,
/// versions, capability grants, and required/optional policy for one app.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppRecipe {
    pub schema_version: u32,
    pub app: RecipeApp,
    #[serde(default)]
    pub extensions: Vec<RecipeExtension>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecipeApp {
    pub id: String,
    pub profile: String,
}

/// One locally enabled package pin. `settings` are inert configuration values
/// available to the application while it constructs its compiled extension;
/// the runtime never interprets them and the browser never receives them.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RecipeExtension {
    pub id: String,
    pub version: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Host services this extension requires, mirroring
    /// [`Extension::requires_services`]. Composition checks exact parity, for
    /// the same reason `capabilities` is checked: a recipe that understates
    /// what an extension needs describes a different application than the one
    /// that will run.
    #[serde(default)]
    pub services: Vec<String>,
    /// Other extensions this one depends on, mirroring
    /// [`Extension::depends_on`]. Each must be enabled earlier in this file.
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub settings: BTreeMap<String, toml::Value>,
}

fn default_true() -> bool {
    true
}

impl AppRecipe {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, CompositionError> {
        let path = path.as_ref();
        let source =
            std::fs::read_to_string(path).map_err(|source| CompositionError::RecipeRead {
                path: path.to_path_buf(),
                source,
            })?;
        let recipe: Self =
            toml::from_str(&source).map_err(|source| CompositionError::RecipeParse {
                path: path.to_path_buf(),
                source,
            })?;
        recipe.validate()?;
        Ok(recipe)
    }

    pub fn validate(&self) -> Result<(), CompositionError> {
        if self.schema_version != 1 {
            return Err(CompositionError::InvalidRecipe(format!(
                "unsupported schema_version {}; expected 1",
                self.schema_version
            )));
        }
        validate_id("app", &self.app.id).map_err(CompositionError::InvalidRecipe)?;
        if !matches!(self.app.profile.as_str(), "web" | "wasm" | "full") {
            return Err(CompositionError::InvalidRecipe(format!(
                "app profile {:?} must be one of: web, wasm, full",
                self.app.profile
            )));
        }
        if self.extensions.iter().all(|extension| !extension.enabled) {
            return Err(CompositionError::InvalidRecipe(
                "at least one extension must be enabled".to_string(),
            ));
        }

        let mut ids = HashSet::new();
        for extension in &self.extensions {
            validate_id("extension", &extension.id).map_err(CompositionError::InvalidRecipe)?;
            if !ids.insert(extension.id.as_str()) {
                return Err(CompositionError::InvalidRecipe(format!(
                    "duplicate extension id {:?}",
                    extension.id
                )));
            }
            if extension.version.trim().is_empty() {
                return Err(CompositionError::InvalidRecipe(format!(
                    "extension {} has an empty version pin",
                    extension.id
                )));
            }
            validate_tokens(
                &format!("extension {} capability", extension.id),
                extension.capabilities.iter().map(String::as_str),
            )
            .map_err(CompositionError::InvalidRecipe)?;
            validate_tokens(
                &format!("extension {} service", extension.id),
                extension.services.iter().map(String::as_str),
            )
            .map_err(CompositionError::InvalidRecipe)?;
            validate_tokens(
                &format!("extension {} dependency", extension.id),
                extension.depends_on.iter().map(String::as_str),
            )
            .map_err(CompositionError::InvalidRecipe)?;
            if extension.depends_on.contains(&extension.id) {
                return Err(CompositionError::InvalidRecipe(format!(
                    "extension {} declares a dependency on itself",
                    extension.id
                )));
            }
        }
        Ok(())
    }

    /// Read an extension's inert settings before constructing its compiled
    /// implementation. This does not enable it or bypass version/capability
    /// validation in [`CompositeSurface::from_recipe`].
    pub fn extension(&self, id: &str) -> Option<&RecipeExtension> {
        self.extensions.iter().find(|extension| extension.id == id)
    }
}

/// One complete server-owned capability. State, actions, routes, browser
/// module metadata, and reconnect behavior remain together instead of being
/// manually flattened into one monolithic `Surface` implementation.
pub trait Extension: Send + Sync + 'static {
    fn id(&self) -> &str;
    fn version(&self) -> &str;
    fn state(&self) -> &dyn SurfaceState;
    fn actions(&self) -> &[ToolDef];

    /// The browser half, if this extension has UI. Server-only extensions may
    /// return `None`; their actions remain agent-visible but are absent from the
    /// browser module allowlist.
    fn client_module(&self) -> Option<ClientModule>;

    /// Capability vocabulary granted by the recipe. For a browser extension,
    /// this must exactly match its `ClientModule.capabilities` so neither the
    /// recipe nor `/extensions` understates what is enabled.
    fn capabilities(&self) -> Vec<&str> {
        Vec::new()
    }

    /// Host services this extension cannot run without.
    ///
    /// Composition refuses to build when any of these has no provider, so a
    /// declared requirement is enforcement rather than documentation. Prefer
    /// declaring a service over accepting a value in the constructor: the
    /// constructor argument works, but it is invisible to the recipe and to
    /// anyone reviewing what the application is made of.
    fn requires_services(&self) -> Vec<ServiceRequirement> {
        Vec::new()
    }

    /// Other extensions this one reads or writes through.
    ///
    /// Composition resolves each against the enabled set at the declared
    /// version, rejects cycles, and requires the dependency to appear earlier
    /// in the recipe so the file reads in dependency order.
    fn depends_on(&self) -> Vec<ExtensionDependency> {
        Vec::new()
    }

    /// Documents this extension is authoritative for. Exactly one extension may
    /// own a given document name.
    fn owns_documents(&self) -> Vec<DocumentClaim> {
        Vec::new()
    }

    /// Documents this extension writes but does not own.
    ///
    /// Composition requires a declared dependency on the owner. Writing another
    /// extension's document without depending on it is the shape the extension
    /// catalog audit found in `same-page-readiness`, where approval advanced a
    /// revision owned by `understanding-projection`.
    fn writes_documents(&self) -> Vec<DocumentClaim> {
        Vec::new()
    }

    /// Resolve declared services into this extension, after composition has
    /// confirmed every requirement has a provider.
    ///
    /// Called once, before the surface is built and before any request is
    /// served. Returning `Err` fails startup: an extension that cannot bind
    /// what it declared must not run in a degraded mode the recipe does not
    /// describe.
    fn bind_services(&self, registry: &ServiceRegistry) -> Result<(), String> {
        let _ = registry;
        Ok(())
    }

    /// Receive the shared documents this composition declared.
    ///
    /// Called once, after ownership has been validated and after
    /// [`Self::bind_services`]. Every extension is offered the registry —
    /// owners take a handle to the document they are authoritative for,
    /// declared writers take a handle to write through. Returning `Err` fails
    /// startup for the same reason a failed service bind does.
    fn bind_documents(&self, documents: &DocumentRegistry) -> Result<(), String> {
        let _ = documents;
        Ok(())
    }

    fn store(&self) -> Option<&dyn SurfaceStore> {
        None
    }

    fn routes(&self) -> Vec<RouteDef> {
        Vec::new()
    }

    /// Mutation routes backed by this extension's existing action
    /// declarations. Composition rejects a route that names another
    /// extension's action or no action at all.
    fn action_routes(&self) -> Vec<ActionRouteDef> {
        Vec::new()
    }

    fn focus_events(&self) -> &[&str] {
        &[]
    }

    /// Whether this extension registers stable semantic target ids under its
    /// own [`Self::id`] namespace.
    fn semantic_targets(&self) -> bool {
        false
    }

    /// Handed the host attention service once, at install time. See
    /// [`Surface::bind_semantic_targets`]: an extension whose tools park on a
    /// wait needs the handle to read the human's live selection from inside a
    /// future that holds no surface reference. Context only, never authority.
    fn bind_semantic_targets(
        &self,
        _service: &std::sync::Arc<crate::semantic_targets::SemanticTargetService>,
    ) {
    }

    /// Told who is about to call an action, immediately before dispatch. See
    /// [`Surface::note_caller`] — an extension that records authorship reads
    /// this to pick a byline. Broadcast to every extension, because the
    /// composite cannot know which one owns the action about to run and
    /// "somebody is calling" is not privileged information.
    fn note_caller(&self, _actor: &crate::Actor) {}

    /// Raw `/ws` frames are not namespaced yet. Exactly one enabled extension
    /// may claim this transport; composition rejects a second claimant rather
    /// than broadcasting ambiguous binary frames to multiple protocols.
    fn binary_transport(&self) -> bool {
        false
    }
}

/// The adapter consumed by the existing runtime. It flattens the action and
/// route registries only after ownership checks pass, while keeping each child
/// state namespaced for description, snapshots, reconnects, and focus routing.
pub struct CompositeSurface {
    extensions: Vec<Arc<dyn Extension>>,
    state: CompositeState,
    store: CompositeStore,
    has_store: bool,
    actions: Vec<ToolDef>,
    modules: Vec<ClientModule>,
}

impl CompositeSurface {
    /// Compose all supplied extensions using their compiled required/optional
    /// module policy. Prefer [`from_recipe`](Self::from_recipe) for an app users
    /// will install or review.
    pub fn try_new(extensions: Vec<Box<dyn Extension>>) -> Result<Self, CompositionError> {
        Self::try_new_with_services(extensions, &ServiceRegistry::new())
    }

    /// [`try_new`](Self::try_new) with host services available for injection.
    /// An extension declaring [`Extension::requires_services`] must be composed
    /// through this or [`from_recipe_with_services`](Self::from_recipe_with_services);
    /// the registry-free entry points supply an empty registry and will fail
    /// closed rather than construct the extension without what it declared.
    pub fn try_new_with_services(
        extensions: Vec<Box<dyn Extension>>,
        services: &ServiceRegistry,
    ) -> Result<Self, CompositionError> {
        let extensions = extensions
            .into_iter()
            .map(|extension| (Arc::<dyn Extension>::from(extension), None))
            .collect();
        Self::build(extensions, services)
    }

    /// Select a pinned subset from locally compiled implementations. The
    /// recipe order becomes the deterministic state/manifest order. Missing
    /// code, version drift, capability drift, duplicate ids, and ownership
    /// collisions all fail before the server binds a socket.
    pub fn from_recipe(
        recipe: &AppRecipe,
        available: Vec<Box<dyn Extension>>,
    ) -> Result<Self, CompositionError> {
        Self::from_recipe_with_services(recipe, available, &ServiceRegistry::new())
    }

    /// [`from_recipe`](Self::from_recipe) with host services available for
    /// injection. In addition to the version and capability checks, the recipe
    /// must declare exactly the services and dependencies the compiled code
    /// declares, every required service must have a provider, and dependencies
    /// must be enabled, acyclic, and listed before their dependents.
    pub fn from_recipe_with_services(
        recipe: &AppRecipe,
        available: Vec<Box<dyn Extension>>,
        services: &ServiceRegistry,
    ) -> Result<Self, CompositionError> {
        recipe.validate()?;
        let mut compiled = HashMap::<String, Box<dyn Extension>>::new();
        for extension in available {
            let id = extension.id().to_string();
            if compiled.insert(id.clone(), extension).is_some() {
                return Err(CompositionError::InvalidExtensions(format!(
                    "duplicate compiled extension id {id:?}"
                )));
            }
        }

        let mut selected = Vec::new();
        for pin in recipe
            .extensions
            .iter()
            .filter(|extension| extension.enabled)
        {
            let extension = compiled.remove(&pin.id).ok_or_else(|| {
                CompositionError::InvalidExtensions(format!(
                    "recipe enables {}@{}, but that extension is not compiled into this app",
                    pin.id, pin.version
                ))
            })?;
            if extension.version() != pin.version {
                return Err(CompositionError::InvalidExtensions(format!(
                    "recipe pins {}@{}, but the compiled extension is {}@{}",
                    pin.id,
                    pin.version,
                    extension.id(),
                    extension.version()
                )));
            }
            let actual = normalized(extension.capabilities().into_iter());
            let granted = normalized(pin.capabilities.iter().map(String::as_str));
            if actual != granted {
                return Err(CompositionError::InvalidExtensions(format!(
                    "recipe capabilities for {} are {:?}, but compiled code declares {:?}",
                    pin.id, granted, actual
                )));
            }

            let required_services = normalized(
                extension
                    .requires_services()
                    .iter()
                    .map(|requirement| requirement.service),
            );
            let declared_services = normalized(pin.services.iter().map(String::as_str));
            if required_services != declared_services {
                return Err(CompositionError::InvalidExtensions(format!(
                    "recipe services for {} are {:?}, but compiled code requires {:?}",
                    pin.id, declared_services, required_services
                )));
            }

            let code_dependencies = normalized(
                extension
                    .depends_on()
                    .iter()
                    .map(|dependency| dependency.extension),
            );
            let recipe_dependencies = normalized(pin.depends_on.iter().map(String::as_str));
            if code_dependencies != recipe_dependencies {
                return Err(CompositionError::InvalidExtensions(format!(
                    "recipe dependencies for {} are {:?}, but compiled code declares {:?}",
                    pin.id, recipe_dependencies, code_dependencies
                )));
            }

            selected.push((Arc::<dyn Extension>::from(extension), Some(pin.required)));
        }
        Self::build(selected, services)
    }

    pub fn extension_ids(&self) -> Vec<&str> {
        self.extensions
            .iter()
            .map(|extension| extension.id())
            .collect()
    }

    fn build(
        extensions: Vec<(Arc<dyn Extension>, Option<bool>)>,
        services: &ServiceRegistry,
    ) -> Result<Self, CompositionError> {
        if extensions.is_empty() {
            return Err(CompositionError::InvalidExtensions(
                "a CompositeSurface needs at least one enabled extension".to_string(),
            ));
        }

        let mut ids = HashSet::new();
        let mut action_names = HashSet::new();
        let mut route_keys = HashSet::new();
        let mut focus_events = HashSet::new();
        let mut module_events = HashSet::new();
        let mut binary_owner: Option<String> = None;
        let mut actions = Vec::new();
        let mut modules = Vec::new();
        let mut children = Vec::new();

        for (extension, required_override) in extensions {
            validate_id("extension", extension.id())
                .map_err(CompositionError::InvalidExtensions)?;
            if extension.version().trim().is_empty() {
                return Err(CompositionError::InvalidExtensions(format!(
                    "extension {} has an empty version",
                    extension.id()
                )));
            }
            if !ids.insert(extension.id().to_string()) {
                return Err(CompositionError::InvalidExtensions(format!(
                    "duplicate extension id {:?}",
                    extension.id()
                )));
            }

            validate_tokens(
                &format!("extension {} capability", extension.id()),
                extension.capabilities().into_iter(),
            )
            .map_err(CompositionError::InvalidExtensions)?;

            let owned_actions: HashSet<String> = extension
                .actions()
                .iter()
                .map(|action| action.name.clone())
                .collect();
            let human_actions: HashSet<String> = extension
                .actions()
                .iter()
                .filter(|action| action.audience.human())
                .map(|action| action.name.clone())
                .collect();
            if owned_actions.len() != extension.actions().len() {
                return Err(CompositionError::InvalidExtensions(format!(
                    "extension {} declares duplicate action names",
                    extension.id()
                )));
            }
            for action in extension.actions() {
                if action.name.trim().is_empty() {
                    return Err(CompositionError::InvalidExtensions(format!(
                        "extension {} declares an empty action name",
                        extension.id()
                    )));
                }
                if !action_names.insert(action.name.clone()) {
                    return Err(CompositionError::InvalidExtensions(format!(
                        "action {:?} is owned by more than one extension",
                        action.name
                    )));
                }
                actions.push(action.clone());
            }

            for route in extension.routes() {
                validate_route(extension.id(), &route, &mut route_keys)?;
            }
            for route in extension.action_routes() {
                validate_action_route(extension.id(), &route, &owned_actions, &mut route_keys)?;
            }
            for event in extension.focus_events() {
                if !focus_events.insert((*event).to_string()) {
                    return Err(CompositionError::InvalidExtensions(format!(
                        "focus event {event:?} is owned by more than one extension"
                    )));
                }
            }

            if extension.binary_transport() {
                if let Some(owner) = &binary_owner {
                    return Err(CompositionError::InvalidExtensions(format!(
                        "extensions {owner} and {} both claim the unnamespaced /ws binary transport",
                        extension.id()
                    )));
                }
                binary_owner = Some(extension.id().to_string());
            }

            if let Some(mut module) = extension.client_module() {
                module
                    .validate()
                    .map_err(CompositionError::InvalidExtensions)?;
                if module.id != extension.id() || module.version != extension.version() {
                    return Err(CompositionError::InvalidExtensions(format!(
                        "extension {}@{} returned browser module {}@{}",
                        extension.id(),
                        extension.version(),
                        module.id,
                        module.version
                    )));
                }
                let module_capabilities =
                    normalized(module.capabilities.iter().map(String::as_str));
                let extension_capabilities = normalized(extension.capabilities().into_iter());
                if module_capabilities != extension_capabilities {
                    return Err(CompositionError::InvalidExtensions(format!(
                        "extension {} capabilities {:?} do not match its browser module {:?}",
                        extension.id(),
                        extension_capabilities,
                        module_capabilities
                    )));
                }
                let declared_actions = module.actions.as_ref().ok_or_else(|| {
                    CompositionError::InvalidExtensions(format!(
                        "extension {} must explicitly declare browser action ownership",
                        extension.id()
                    ))
                })?;
                let declared_actions: HashSet<String> = declared_actions.iter().cloned().collect();
                if declared_actions != human_actions {
                    return Err(CompositionError::InvalidExtensions(format!(
                        "extension {} browser actions {:?} do not match its human-visible actions {:?}",
                        extension.id(),
                        declared_actions,
                        human_actions
                    )));
                }
                for event in &module.events {
                    if !module_events.insert(event.clone()) {
                        return Err(CompositionError::InvalidExtensions(format!(
                            "browser event {event:?} is declared by more than one extension"
                        )));
                    }
                }
                if let Some(required) = required_override {
                    module.required = required;
                }
                modules.push(module);
            } else if required_override == Some(false) {
                // Required/optional is browser load policy. A server-only
                // extension has no fallible client load to downgrade.
                return Err(CompositionError::InvalidExtensions(format!(
                    "server-only extension {} cannot be marked optional",
                    extension.id()
                )));
            }

            children.push(extension);
        }

        validate_dependency_graph(&children)?;
        let documents = validate_document_ownership(&children)?;
        bind_declared_services(&children, services)?;
        for extension in &children {
            extension.bind_documents(&documents).map_err(|error| {
                CompositionError::InvalidExtensions(format!(
                    "extension {} could not bind its shared documents: {error}",
                    extension.id()
                ))
            })?;
        }

        let has_store = children.iter().any(|extension| extension.store().is_some());
        let state = CompositeState {
            extensions: children.clone(),
        };
        let store = CompositeStore {
            extensions: children.clone(),
        };
        Ok(Self {
            extensions: children,
            state,
            store,
            has_store,
            actions,
            modules,
        })
    }
}

impl Surface for CompositeSurface {
    fn state(&self) -> &dyn SurfaceState {
        &self.state
    }

    fn tools(&self) -> &[ToolDef] {
        &self.actions
    }

    fn note_caller(&self, actor: &crate::Actor) {
        for extension in &self.extensions {
            extension.note_caller(actor);
        }
    }

    fn bind_semantic_targets(
        &self,
        service: &std::sync::Arc<crate::semantic_targets::SemanticTargetService>,
    ) {
        for extension in &self.extensions {
            extension.bind_semantic_targets(service);
        }
    }

    fn client_modules(&self) -> Vec<ClientModule> {
        self.modules.clone()
    }

    fn store(&self) -> Option<&dyn SurfaceStore> {
        self.has_store.then_some(&self.store)
    }

    fn routes(&self) -> Vec<RouteDef> {
        self.extensions
            .iter()
            .flat_map(|extension| extension.routes())
            .collect()
    }

    fn action_routes(&self) -> Vec<ActionRouteDef> {
        self.extensions
            .iter()
            .flat_map(|extension| extension.action_routes())
            .collect()
    }

    fn resolve_focus(&self, event: &str, id: &str) -> Result<Option<String>, String> {
        let Some(extension) = self
            .extensions
            .iter()
            .find(|extension| extension.focus_events().contains(&event))
        else {
            return Ok(None);
        };
        extension.state().resolve(id).map_err(|error| {
            format!(
                "extension `{}` focus resolution failed: {error}",
                extension.id()
            )
        })
    }

    fn semantic_target_namespaces(&self) -> Vec<String> {
        self.extensions
            .iter()
            .filter(|extension| extension.semantic_targets())
            .map(|extension| extension.id().to_string())
            .collect()
    }

    fn resolve_semantic_target(
        &self,
        target: &SemanticTargetRef,
    ) -> Result<Option<SemanticTarget>, String> {
        let Some(extension) = self.extensions.iter().find(|extension| {
            extension.semantic_targets() && extension.id() == target.extension_id
        }) else {
            return Ok(None);
        };
        extension.state().semantic_target(target).map_err(|error| {
            format!(
                "extension `{}` semantic target resolution failed: {error}",
                extension.id()
            )
        })
    }
}

struct CompositeState {
    extensions: Vec<Arc<dyn Extension>>,
}

impl SurfaceState for CompositeState {
    fn backing(&self) -> StateBacking {
        StateBacking::Composite
    }

    fn describe(&self) -> Result<String, String> {
        let mut descriptions = Vec::new();
        for extension in &self.extensions {
            let description = extension.state().describe().map_err(|error| {
                format!("extension `{}` describe failed: {error}", extension.id())
            })?;
            if !description.trim().is_empty() {
                descriptions.push(format!("[{}]\n{}", extension.id(), description.trim()));
            }
        }
        Ok(descriptions.join("\n\n"))
    }

    fn snapshot_png(&self) -> crate::SnapshotPngFuture<'_> {
        Box::pin(async move {
            for extension in &self.extensions {
                if let Some(png) = extension.state().snapshot_png().await.map_err(|error| {
                    format!("extension `{}` render failed: {error}", extension.id())
                })? {
                    return Ok(Some(png));
                }
            }
            Ok(None)
        })
    }

    fn snapshot(&self) -> Result<StateSnapshot, String> {
        let mut states = JsonMap::new();
        let mut chrome = JsonMap::new();
        for extension in &self.extensions {
            let snapshot = extension.state().snapshot().map_err(|error| {
                format!("extension `{}` snapshot failed: {error}", extension.id())
            })?;
            states.insert(
                extension.id().to_string(),
                json!({
                    "backing": backing_name(snapshot.backing),
                    "body": snapshot.body,
                }),
            );
            if let Some(value) = snapshot.chrome {
                chrome.insert(extension.id().to_string(), value);
            }
        }
        Ok(StateSnapshot {
            backing: StateBacking::Composite,
            body: json!({ "schemaVersion": 1, "extensions": states }),
            chrome: (!chrome.is_empty()).then_some(JsonValue::Object(chrome)),
        })
    }

    fn activity_state_revision(&self) -> Result<Vec<crate::ActivityStateRevision>, String> {
        let mut revisions = Vec::new();
        for extension in &self.extensions {
            let extension_revisions =
                extension
                    .state()
                    .activity_state_revision()
                    .map_err(|error| {
                        format!(
                            "extension `{}` activity revision failed: {error}",
                            extension.id()
                        )
                    })?;
            revisions.extend(
                extension_revisions
                    .into_iter()
                    .map(|revision| revision.namespaced(extension.id())),
            );
        }
        Ok(revisions)
    }

    fn resolve(&self, id: &str) -> Result<Option<String>, String> {
        for extension in &self.extensions {
            if let Some(description) = extension.state().resolve(id).map_err(|error| {
                format!("extension `{}` resolve failed: {error}", extension.id())
            })? {
                return Ok(Some(description));
            }
        }
        Ok(None)
    }

    fn ws_hello(&self) -> Result<Vec<Vec<u8>>, String> {
        let mut extensions = self
            .extensions
            .iter()
            .filter(|extension| extension.binary_transport())
            .peekable();
        if extensions.peek().is_none() {
            return Err(crate::BINARY_TRANSPORT_NOT_IMPLEMENTED.to_string());
        }
        let mut frames = Vec::new();
        for extension in extensions {
            frames.extend(extension.state().ws_hello().map_err(|error| {
                format!("extension `{}` replay failed: {error}", extension.id())
            })?);
        }
        Ok(frames)
    }

    fn ws_receive(&self, data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        let mut extensions = self
            .extensions
            .iter()
            .filter(|extension| extension.binary_transport())
            .peekable();
        if extensions.peek().is_none() {
            return Err(crate::BINARY_TRANSPORT_NOT_IMPLEMENTED.to_string());
        }
        let mut frames = Vec::new();
        for extension in extensions {
            frames.extend(extension.state().ws_receive(data).map_err(|error| {
                format!("extension `{}` receive failed: {error}", extension.id())
            })?);
        }
        Ok(frames)
    }

    fn reconnect_events(&self) -> Vec<(String, JsonValue)> {
        self.extensions
            .iter()
            .flat_map(|extension| extension.state().reconnect_events())
            .collect()
    }
}

struct CompositeStore {
    extensions: Vec<Arc<dyn Extension>>,
}

impl SurfaceStore for CompositeStore {
    fn context(&self) -> String {
        self.extensions
            .iter()
            .filter_map(|extension| {
                let context = extension.store()?.context();
                (!context.trim().is_empty())
                    .then(|| format!("[{}]\n{}", extension.id(), context.trim()))
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

const CORE_ROUTES: &[&str] = &[
    "/_agui/client.js",
    "/_agui/provider-settings.js",
    "/_agui/provider-settings.css",
    "/_agui/conversation.js",
    "/_agui/conversation.css",
    "/_agui/semantic-targets.js",
    "/_agui/semantic-targets.css",
    "/extensions",
    "/activity",
    "/activity/events",
    "/mcp",
    "/ws",
    "/events",
    "/debug/stats",
    "/control",
    "/ask",
    "/surface/action",
    "/canvas-tool",
    "/semantic",
    "/provider",
    "/auth",
    "/connections",
    "/models/openrouter",
    "/interrupt",
    "/decision",
    "/surface/state",
    "/canvas-state",
    "/canvas.png",
    "/canvas-image",
    "/semantic-targets",
    "/semantic-targets/presence",
    "/semantic-targets/focus",
    "/semantic-targets/clear",
];

/// Resolve every declared dependency against the enabled set.
///
/// Ordering is validated rather than repaired. Composition could topologically
/// sort the children and silently proceed, but recipe order is the documented
/// state, manifest, and snapshot order, so a reordering composite would run an
/// application whose parts appear in a different order than the file a reviewer
/// approved. An out-of-order recipe is a two-line edit; a recipe that no longer
/// describes execution order is a standing misdescription.
fn validate_dependency_graph(children: &[Arc<dyn Extension>]) -> Result<(), CompositionError> {
    let position: HashMap<&str, usize> = children
        .iter()
        .enumerate()
        .map(|(index, extension)| (extension.id(), index))
        .collect();

    for (index, extension) in children.iter().enumerate() {
        let mut seen = HashSet::new();
        for dependency in extension.depends_on() {
            if dependency.extension == extension.id() {
                return Err(CompositionError::UnsatisfiedDependency(format!(
                    "extension {} depends on itself",
                    extension.id()
                )));
            }
            if !seen.insert(dependency.extension) {
                return Err(CompositionError::UnsatisfiedDependency(format!(
                    "extension {} declares dependency {:?} twice",
                    extension.id(),
                    dependency.extension
                )));
            }
            if dependency.reason.trim().is_empty() {
                return Err(CompositionError::UnsatisfiedDependency(format!(
                    "extension {} depends on {} without stating why",
                    extension.id(),
                    dependency.extension
                )));
            }

            let Some(&dependency_index) = position.get(dependency.extension) else {
                return Err(CompositionError::UnsatisfiedDependency(format!(
                    "extension {} depends on {}@{} ({}), which is not enabled in this composition",
                    extension.id(),
                    dependency.extension,
                    dependency.version,
                    dependency.reason
                )));
            };
            let provider = &children[dependency_index];
            if provider.version() != dependency.version {
                return Err(CompositionError::UnsatisfiedDependency(format!(
                    "extension {} depends on {}@{}, but the composed {} is @{}",
                    extension.id(),
                    dependency.extension,
                    dependency.version,
                    dependency.extension,
                    provider.version()
                )));
            }
            // A dependency later in the list is either a cycle or a recipe
            // whose order contradicts its own declarations. Both are reported
            // here, because both mean the composed order cannot be read off
            // the file.
            if dependency_index > index {
                return Err(CompositionError::UnsatisfiedDependency(format!(
                    "extension {} depends on {}, which is composed after it; \
                     move {} earlier so the recipe reads in dependency order",
                    extension.id(),
                    dependency.extension,
                    dependency.extension
                )));
            }
        }
    }
    Ok(())
}

/// Exactly one owner per document, and no writer without a declared dependency
/// on that owner. Returns the registry those claims describe.
fn validate_document_ownership(
    children: &[Arc<dyn Extension>],
) -> Result<DocumentRegistry, CompositionError> {
    let mut owners: HashMap<&str, &str> = HashMap::new();
    let mut registry = DocumentRegistry::new();
    for extension in children {
        for claim in extension.owns_documents() {
            if claim.reason.trim().is_empty() {
                return Err(CompositionError::InvalidExtensions(format!(
                    "extension {} owns document {:?} without stating what it does with it",
                    extension.id(),
                    claim.document
                )));
            }
            if let Some(existing) = owners.insert(claim.document, extension.id()) {
                return Err(CompositionError::InvalidExtensions(format!(
                    "document {:?} is owned by both {} and {}; a document with two owners \
                     has no authoritative revision",
                    claim.document,
                    existing,
                    extension.id()
                )));
            }
            registry.declare(claim.document, extension.id());
        }
    }

    for extension in children {
        let dependencies: HashSet<&str> = extension
            .depends_on()
            .iter()
            .map(|dependency| dependency.extension)
            .collect();
        for claim in extension.writes_documents() {
            let Some(&owner) = owners.get(claim.document) else {
                return Err(CompositionError::InvalidExtensions(format!(
                    "extension {} writes document {:?}, which no composed extension owns",
                    extension.id(),
                    claim.document
                )));
            };
            if owner == extension.id() {
                return Err(CompositionError::InvalidExtensions(format!(
                    "extension {} lists document {:?} as both owned and written; \
                     an owner's own writes are not a cross-extension dependency",
                    extension.id(),
                    claim.document
                )));
            }
            if !dependencies.contains(owner) {
                return Err(CompositionError::InvalidExtensions(format!(
                    "extension {} writes document {:?} owned by {}, without declaring a \
                     dependency on {}",
                    extension.id(),
                    claim.document,
                    owner,
                    owner
                )));
            }
        }
    }
    Ok(registry)
}

/// Fail closed on a declared service with no provider, then hand every
/// extension the registry exactly once.
fn bind_declared_services(
    children: &[Arc<dyn Extension>],
    services: &ServiceRegistry,
) -> Result<(), CompositionError> {
    for extension in children {
        let mut seen = HashSet::new();
        for requirement in extension.requires_services() {
            if !seen.insert(requirement.service) {
                return Err(CompositionError::InvalidExtensions(format!(
                    "extension {} requires host service {:?} twice",
                    extension.id(),
                    requirement.service
                )));
            }
            if requirement.reason.trim().is_empty() {
                return Err(CompositionError::InvalidExtensions(format!(
                    "extension {} requires host service {:?} without stating why",
                    extension.id(),
                    requirement.service
                )));
            }
            if !services.provides(requirement.service) {
                return Err(CompositionError::MissingService {
                    extension: extension.id().to_string(),
                    service: requirement.service.to_string(),
                    reason: requirement.reason.to_string(),
                    available: services.names().into_iter().map(str::to_string).collect(),
                });
            }
        }
    }

    // Binding happens only after every requirement across the whole
    // composition is known to have a provider, so a partially-bound
    // extension set cannot exist even briefly.
    for extension in children {
        extension.bind_services(services).map_err(|error| {
            CompositionError::InvalidExtensions(format!(
                "extension {} could not bind its declared services: {error}",
                extension.id()
            ))
        })?;
    }
    Ok(())
}

fn validate_route(
    extension_id: &str,
    route: &RouteDef,
    seen: &mut HashSet<(u8, &'static str)>,
) -> Result<(), CompositionError> {
    validate_route_key(extension_id, route.method, route.path, seen)
}

fn validate_action_route(
    extension_id: &str,
    route: &ActionRouteDef,
    owned_actions: &HashSet<String>,
    seen: &mut HashSet<(u8, &'static str)>,
) -> Result<(), CompositionError> {
    if route.method == HttpMethod::Get {
        return Err(CompositionError::InvalidExtensions(format!(
            "extension {extension_id} action route {:?} must use a mutating HTTP method",
            route.path
        )));
    }
    if !owned_actions.contains(route.action) {
        return Err(CompositionError::InvalidExtensions(format!(
            "extension {extension_id} action route {:?} names action {:?}, which that extension does not own",
            route.path, route.action
        )));
    }
    validate_route_key(extension_id, route.method, route.path, seen)
}

fn validate_route_key(
    extension_id: &str,
    method: HttpMethod,
    path: &'static str,
    seen: &mut HashSet<(u8, &'static str)>,
) -> Result<(), CompositionError> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains("..")
        || path.contains('\\')
    {
        return Err(CompositionError::InvalidExtensions(format!(
            "extension {extension_id} route {:?} must be a same-origin absolute path",
            path
        )));
    }
    if CORE_ROUTES.contains(&path) {
        return Err(CompositionError::InvalidExtensions(format!(
            "extension {extension_id} route {:?} collides with a core runtime route",
            path
        )));
    }
    let method_key = match method {
        HttpMethod::Get => 0,
        HttpMethod::Post => 1,
        HttpMethod::Put => 2,
        HttpMethod::Delete => 3,
    };
    if !seen.insert((method_key, path)) {
        return Err(CompositionError::InvalidExtensions(format!(
            "route {} {:?} is owned by more than one extension",
            method_name(method),
            path
        )));
    }
    Ok(())
}

fn method_name(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Delete => "DELETE",
    }
}

fn validate_id(kind: &str, id: &str) -> Result<(), String> {
    if id.is_empty()
        || !id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        return Err(format!(
            "{kind} id {id:?} must contain only lowercase ASCII letters, digits, '.', '_' or '-'"
        ));
    }
    Ok(())
}

fn validate_tokens<'a>(label: &str, values: impl Iterator<Item = &'a str>) -> Result<(), String> {
    let mut seen = HashSet::new();
    for value in values {
        if value.trim().is_empty() || !seen.insert(value) {
            return Err(format!("{label} is empty or duplicated: {value:?}"));
        }
    }
    Ok(())
}

fn normalized<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut values: Vec<String> = values.map(str::to_string).collect();
    values.sort();
    values
}

fn backing_name(backing: StateBacking) -> &'static str {
    match backing {
        StateBacking::Crdt => "crdt",
        StateBacking::LastWriterWins => "lastWriterWins",
        StateBacking::RevisionHistory => "revisionHistory",
        StateBacking::Ephemeral => "ephemeral",
        StateBacking::Composite => "composite",
    }
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;

    use super::*;
    use crate::{Effect, RouteRequest, RouteResponse};

    struct TestState {
        label: &'static str,
        value: Mutex<i64>,
    }

    impl SurfaceState for TestState {
        fn backing(&self) -> StateBacking {
            StateBacking::LastWriterWins
        }

        fn describe(&self) -> Result<String, String> {
            Ok(format!("{} = {}", self.label, self.value.lock()))
        }

        fn snapshot(&self) -> Result<StateSnapshot, String> {
            Ok(StateSnapshot {
                backing: StateBacking::LastWriterWins,
                body: json!({ "value": *self.value.lock() }),
                chrome: None,
            })
        }

        fn reconnect_events(&self) -> Vec<(String, JsonValue)> {
            vec![(
                format!("{}.state", self.label),
                self.snapshot().expect("read test extension snapshot").body,
            )]
        }
    }

    struct TestExtension {
        id: &'static str,
        version: &'static str,
        state: Arc<TestState>,
        actions: Vec<ToolDef>,
        route: Option<&'static str>,
        action_route: Option<(&'static str, &'static str)>,
        browser: bool,
        requires: Vec<ServiceRequirement>,
        depends: Vec<ExtensionDependency>,
        owns: Vec<DocumentClaim>,
        writes: Vec<DocumentClaim>,
        /// Set by `bind_services`, so a test can prove injection happened
        /// rather than only that composition did not error.
        bound: Arc<Mutex<Vec<String>>>,
    }

    impl TestExtension {
        fn new(id: &'static str, action_name: &'static str, route: Option<&'static str>) -> Self {
            let state = Arc::new(TestState {
                label: id,
                value: Mutex::new(0),
            });
            let actions = vec![ToolDef::new(
                action_name,
                "increment test state",
                json!({ "type": "object", "properties": {} }),
                {
                    let state = state.clone();
                    move |_args| {
                        let state = state.clone();
                        Effect::Mutate(Box::new(move |_surface| {
                            *state.value.lock() += 1;
                            Ok(None)
                        }))
                    }
                },
            )];
            Self {
                id,
                version: "1.0.0",
                state,
                actions,
                route,
                action_route: None,
                browser: true,
                requires: Vec::new(),
                depends: Vec::new(),
                owns: Vec::new(),
                writes: Vec::new(),
                bound: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn action_route(mut self, path: &'static str, action: &'static str) -> Self {
            self.action_route = Some((path, action));
            self
        }

        fn server_only(mut self) -> Self {
            self.browser = false;
            self
        }

        fn requires(mut self, service: &'static str) -> Self {
            self.requires
                .push(ServiceRequirement::new(service, "a test needs it"));
            self
        }

        fn requires_unexplained(mut self, service: &'static str) -> Self {
            self.requires.push(ServiceRequirement::new(service, "  "));
            self
        }

        fn depends(mut self, extension: &'static str, version: &'static str) -> Self {
            self.depends.push(ExtensionDependency::new(
                extension,
                version,
                "a test depends on it",
            ));
            self
        }

        fn owns(mut self, document: &'static str) -> Self {
            self.owns
                .push(DocumentClaim::new(document, "a test owns it"));
            self
        }

        fn writes(mut self, document: &'static str) -> Self {
            self.writes
                .push(DocumentClaim::new(document, "a test writes it"));
            self
        }

        fn bound(&self) -> Arc<Mutex<Vec<String>>> {
            self.bound.clone()
        }
    }

    impl Extension for TestExtension {
        fn id(&self) -> &str {
            self.id
        }

        fn version(&self) -> &str {
            self.version
        }

        fn state(&self) -> &dyn SurfaceState {
            self.state.as_ref()
        }

        fn actions(&self) -> &[ToolDef] {
            &self.actions
        }

        fn client_module(&self) -> Option<ClientModule> {
            if !self.browser {
                return None;
            }
            Some(
                ClientModule::lazy(
                    self.id,
                    self.version,
                    format!("/extensions/{}/index.js", self.id),
                    format!("{}-mount", self.id),
                )
                .action(self.actions[0].name.clone())
                .event(format!("{}.state", self.id))
                .capability("dom"),
            )
        }

        fn capabilities(&self) -> Vec<&str> {
            if self.browser {
                vec!["dom"]
            } else {
                Vec::new()
            }
        }

        fn requires_services(&self) -> Vec<ServiceRequirement> {
            self.requires.clone()
        }

        fn depends_on(&self) -> Vec<ExtensionDependency> {
            self.depends.clone()
        }

        fn owns_documents(&self) -> Vec<DocumentClaim> {
            self.owns.clone()
        }

        fn writes_documents(&self) -> Vec<DocumentClaim> {
            self.writes.clone()
        }

        fn bind_services(&self, registry: &ServiceRegistry) -> Result<(), String> {
            for requirement in &self.requires {
                let value = registry.resolve::<TestService>(requirement.service)?;
                self.bound
                    .lock()
                    .push(format!("{}={}", requirement.service, value.0));
            }
            Ok(())
        }

        fn bind_documents(&self, documents: &DocumentRegistry) -> Result<(), String> {
            for claim in self.owns.iter().chain(self.writes.iter()) {
                let document = documents.get(claim.document)?;
                self.bound
                    .lock()
                    .push(format!("doc:{}@{}", document.name(), document.owner()));
            }
            Ok(())
        }

        fn routes(&self) -> Vec<RouteDef> {
            self.route
                .map(|path| RouteDef {
                    method: HttpMethod::Get,
                    path,
                    handler: Box::new(|_request: RouteRequest| {
                        Box::pin(async { RouteResponse::json(200, json!({ "ok": true })) })
                    }),
                })
                .into_iter()
                .collect()
        }

        fn action_routes(&self) -> Vec<ActionRouteDef> {
            self.action_route
                .map(|(path, action)| {
                    ActionRouteDef::post(path, action, |_request| {
                        Box::pin(async { RouteResponse::json(200, json!({ "ok": true })) })
                    })
                })
                .into_iter()
                .collect()
        }
    }

    struct TestService(&'static str);

    fn recipe(source: &str) -> AppRecipe {
        let recipe: AppRecipe = toml::from_str(source).unwrap();
        recipe.validate().unwrap();
        recipe
    }

    fn registry(entries: &[(&str, &'static str)]) -> ServiceRegistry {
        let mut registry = ServiceRegistry::new();
        for (name, value) in entries {
            registry
                .provide(*name, Arc::new(TestService(value)))
                .expect("provide a test service");
        }
        registry
    }

    #[test]
    fn a_declared_service_is_injected_and_a_missing_one_refuses_startup() {
        let extension = TestExtension::new("reader", "read_repo", None).requires("repository");
        let bound = extension.bound();
        CompositeSurface::try_new_with_services(
            vec![Box::new(extension)],
            &registry(&[("repository", "/workspace")]),
        )
        .expect("a satisfied requirement composes");
        assert_eq!(*bound.lock(), vec!["repository=/workspace".to_string()]);

        // Same extension, no provider: startup fails and names the fix.
        let error = CompositeSurface::try_new(vec![Box::new(
            TestExtension::new("reader", "read_repo", None).requires("repository"),
        )])
        .err()
        .expect("an unprovided service must refuse composition");
        assert!(
            matches!(error, CompositionError::MissingService { .. }),
            "{error}"
        );
        let message = error.to_string();
        assert!(
            message.contains("requires host service \"repository\""),
            "{message}"
        );
        assert!(message.contains("does not provide"), "{message}");
    }

    #[test]
    fn binding_is_all_or_nothing_across_the_composition() {
        // `alpha`'s requirement is satisfied and `beta`'s is not. Because
        // resolution is checked for every extension before any is bound, alpha
        // must not observe an injection from a composition that cannot start.
        let alpha = TestExtension::new("alpha", "set_alpha", None).requires("repository");
        let alpha_bound = alpha.bound();
        let beta = TestExtension::new("beta", "set_beta", None).requires("telemetry");

        let error = CompositeSurface::try_new_with_services(
            vec![Box::new(alpha), Box::new(beta)],
            &registry(&[("repository", "/workspace")]),
        )
        .err()
        .expect("one unprovided service must refuse the whole composition");
        assert!(error.to_string().contains("telemetry"), "{error}");
        assert!(
            alpha_bound.lock().is_empty(),
            "a failed composition must not leave an extension bound: {:?}",
            alpha_bound.lock()
        );
    }

    #[test]
    fn a_service_requirement_must_explain_itself() {
        let error = CompositeSurface::try_new_with_services(
            vec![Box::new(
                TestExtension::new("reader", "read_repo", None).requires_unexplained("repository"),
            )],
            &registry(&[("repository", "/workspace")]),
        )
        .err()
        .expect("an unexplained requirement must not compose");
        assert!(error.to_string().contains("without stating why"), "{error}");
    }

    #[test]
    fn a_dependency_must_be_enabled_at_the_declared_version_and_ordered_first() {
        // Satisfied.
        CompositeSurface::try_new(vec![
            Box::new(TestExtension::new("projection", "publish", None)),
            Box::new(
                TestExtension::new("alignment", "approve", None).depends("projection", "1.0.0"),
            ),
        ])
        .expect("a satisfied dependency composes");

        let absent = CompositeSurface::try_new(vec![Box::new(
            TestExtension::new("alignment", "approve", None).depends("projection", "1.0.0"),
        )])
        .err()
        .expect("a dependency on a disabled extension must refuse composition");
        assert!(absent.to_string().contains("not enabled"), "{absent}");

        let drift = CompositeSurface::try_new(vec![
            Box::new(TestExtension::new("projection", "publish", None)),
            Box::new(
                TestExtension::new("alignment", "approve", None).depends("projection", "2.0.0"),
            ),
        ])
        .err()
        .expect("a dependency at the wrong version must refuse composition");
        assert!(drift.to_string().contains("but the composed"), "{drift}");

        let out_of_order = CompositeSurface::try_new(vec![
            Box::new(
                TestExtension::new("alignment", "approve", None).depends("projection", "1.0.0"),
            ),
            Box::new(TestExtension::new("projection", "publish", None)),
        ])
        .err()
        .expect("a dependency composed after its dependent must be rejected");
        assert!(
            out_of_order.to_string().contains("composed after it"),
            "{out_of_order}"
        );
    }

    #[test]
    fn a_dependency_cycle_cannot_compose_in_any_order() {
        // In any linear ordering of a cyclic graph at least one edge points
        // forward, so the "dependency must be composed earlier" rule catches
        // every cycle, not only adjacent pairs. Both orderings of a 2-cycle and
        // one 3-cycle are checked rather than asserting that property.
        let alpha_first = || {
            vec![
                Box::new(TestExtension::new("alpha", "set_alpha", None).depends("beta", "1.0.0"))
                    as Box<dyn Extension>,
                Box::new(TestExtension::new("beta", "set_beta", None).depends("alpha", "1.0.0")),
            ]
        };
        let beta_first = || {
            vec![
                Box::new(TestExtension::new("beta", "set_beta", None).depends("alpha", "1.0.0"))
                    as Box<dyn Extension>,
                Box::new(TestExtension::new("alpha", "set_alpha", None).depends("beta", "1.0.0")),
            ]
        };
        let three_cycle = || {
            vec![
                Box::new(TestExtension::new("alpha", "set_alpha", None).depends("beta", "1.0.0"))
                    as Box<dyn Extension>,
                Box::new(TestExtension::new("beta", "set_beta", None).depends("gamma", "1.0.0")),
                Box::new(TestExtension::new("gamma", "set_gamma", None).depends("alpha", "1.0.0")),
            ]
        };

        for (label, build) in [
            (
                "alpha first",
                Box::new(alpha_first) as Box<dyn Fn() -> Vec<Box<dyn Extension>>>,
            ),
            ("beta first", Box::new(beta_first)),
            ("three cycle", Box::new(three_cycle)),
        ] {
            let error = CompositeSurface::try_new(build())
                .err()
                .unwrap_or_else(|| panic!("{label}: a dependency cycle must refuse composition"));
            assert!(
                error.to_string().contains("composed after it"),
                "{label}: {error}"
            );
        }
    }

    #[test]
    fn a_document_has_one_owner_and_writers_must_depend_on_it() {
        // The shape ADR 0009 admits: one owner, one declared dependent writer.
        CompositeSurface::try_new(vec![
            Box::new(TestExtension::new("projection", "publish", None).owns("studio")),
            Box::new(
                TestExtension::new("alignment", "approve", None)
                    .depends("projection", "1.0.0")
                    .writes("studio"),
            ),
        ])
        .expect("a declared cross-extension write composes");

        // The shape it rejects: `same-page-readiness` advancing a revision
        // owned by `understanding-projection` without saying so.
        let undeclared = CompositeSurface::try_new(vec![
            Box::new(TestExtension::new("projection", "publish", None).owns("studio")),
            Box::new(TestExtension::new("alignment", "approve", None).writes("studio")),
        ])
        .err()
        .expect("writing another extension's document undeclared must be rejected");
        assert!(
            undeclared
                .to_string()
                .contains("without declaring a dependency"),
            "{undeclared}"
        );

        let two_owners = CompositeSurface::try_new(vec![
            Box::new(TestExtension::new("projection", "publish", None).owns("studio")),
            Box::new(TestExtension::new("readiness", "approve", None).owns("studio")),
        ])
        .err()
        .expect("two owners of one document must be rejected");
        assert!(
            two_owners
                .to_string()
                .contains("has no authoritative revision"),
            "{two_owners}"
        );

        let orphan = CompositeSurface::try_new(vec![Box::new(
            TestExtension::new("alignment", "approve", None).writes("studio"),
        )])
        .err()
        .expect("writing an unowned document must be rejected");
        assert!(
            orphan.to_string().contains("no composed extension owns"),
            "{orphan}"
        );
    }

    #[test]
    fn owner_and_declared_writer_both_receive_the_same_document() {
        // The wiring proof for the transactional half: composition builds the
        // registry from validated ownership, and both the owner and the
        // extension that declared a write get a handle naming the same single
        // owner. Neither constructed it, so neither could have named itself.
        let projection = TestExtension::new("projection", "publish", None).owns("studio");
        let alignment = TestExtension::new("alignment", "approve", None)
            .depends("projection", "1.0.0")
            .writes("studio");
        let projection_bound = projection.bound();
        let alignment_bound = alignment.bound();

        CompositeSurface::try_new(vec![Box::new(projection), Box::new(alignment)])
            .expect("a declared owner and writer compose");

        assert_eq!(
            *projection_bound.lock(),
            vec!["doc:studio@projection".to_string()]
        );
        assert_eq!(
            *alignment_bound.lock(),
            vec!["doc:studio@projection".to_string()]
        );
    }

    #[test]
    fn the_recipe_must_declare_what_the_code_requires() {
        let understated = recipe(
            r#"
schema_version = 1
[app]
id = "test-app"
profile = "web"
[[extensions]]
id = "reader"
version = "1.0.0"
capabilities = ["dom"]
"#,
        );
        let error = CompositeSurface::from_recipe_with_services(
            &understated,
            vec![Box::new(
                TestExtension::new("reader", "read_repo", None).requires("repository"),
            )],
            &registry(&[("repository", "/workspace")]),
        )
        .err()
        .expect("a recipe that omits a required service must be rejected");
        assert!(error.to_string().contains("recipe services"), "{error}");

        let overstated = recipe(
            r#"
schema_version = 1
[app]
id = "test-app"
profile = "web"
[[extensions]]
id = "reader"
version = "1.0.0"
capabilities = ["dom"]
depends_on = ["projection"]
"#,
        );
        let error = CompositeSurface::from_recipe_with_services(
            &overstated,
            vec![Box::new(TestExtension::new("reader", "read_repo", None))],
            &ServiceRegistry::new(),
        )
        .err()
        .expect("a recipe claiming a dependency the code does not declare must be rejected");
        assert!(error.to_string().contains("recipe dependencies"), "{error}");

        // Declared on both sides, provided by the host: composes.
        let honest = recipe(
            r#"
schema_version = 1
[app]
id = "test-app"
profile = "web"
[[extensions]]
id = "reader"
version = "1.0.0"
capabilities = ["dom"]
services = ["repository"]
"#,
        );
        CompositeSurface::from_recipe_with_services(
            &honest,
            vec![Box::new(
                TestExtension::new("reader", "read_repo", None).requires("repository"),
            )],
            &registry(&[("repository", "/workspace")]),
        )
        .expect("a recipe matching the code composes");
    }

    #[test]
    fn recipe_selects_pinned_extensions_and_composes_state() {
        let recipe = recipe(
            r#"
schema_version = 1

[app]
id = "test-app"
profile = "web"

[[extensions]]
id = "alpha"
version = "1.0.0"
capabilities = ["dom"]

[[extensions]]
id = "beta"
version = "1.0.0"
capabilities = ["dom"]
"#,
        );
        let surface = CompositeSurface::from_recipe(
            &recipe,
            vec![
                Box::new(TestExtension::new("beta", "set_beta", Some("/beta"))),
                Box::new(TestExtension::new("alpha", "set_alpha", Some("/alpha"))),
            ],
        )
        .unwrap();

        assert_eq!(surface.extension_ids(), vec!["alpha", "beta"]);
        assert_eq!(
            surface
                .tools()
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["set_alpha", "set_beta"]
        );
        assert_eq!(surface.client_modules()[0].id, "alpha");
        let description = surface
            .state()
            .describe()
            .expect("compose test surface description");
        assert!(description.contains("[alpha]\nalpha = 0"));
        assert!(description.contains("[beta]\nbeta = 0"));
        let snapshot = surface
            .state()
            .snapshot()
            .expect("compose test surface snapshot");
        assert_eq!(snapshot.backing, StateBacking::Composite);
        assert_eq!(snapshot.body["extensions"]["alpha"]["body"]["value"], 0);
        assert_eq!(surface.state().reconnect_events().len(), 2);

        let alpha = surface
            .tools()
            .iter()
            .find(|tool| tool.name == "set_alpha")
            .unwrap();
        match (alpha.apply)(&json!({})) {
            Effect::Mutate(apply) => {
                assert_eq!(apply(surface.state()), Ok(None));
            }
            _ => panic!("expected mutate"),
        }
        assert!(surface
            .state()
            .describe()
            .expect("compose updated test surface description")
            .contains("alpha = 1"));
    }

    #[test]
    fn server_only_extensions_compose_without_inventing_a_browser_module() {
        let surface = CompositeSurface::try_new(vec![Box::new(
            TestExtension::new("worker", "run_worker", None).server_only(),
        )])
        .expect("a server-only extension is a valid composition");

        assert_eq!(surface.extension_ids(), ["worker"]);
        assert!(surface.client_modules().is_empty());
        assert_eq!(surface.tools().len(), 1);
        assert_eq!(
            surface
                .state()
                .ws_hello()
                .expect_err("composition without a binary owner must reject /ws"),
            crate::BINARY_TRANSPORT_NOT_IMPLEMENTED
        );
        assert_eq!(
            surface
                .state()
                .ws_receive(b"work")
                .expect_err("composition without a binary owner must reject input"),
            crate::BINARY_TRANSPORT_NOT_IMPLEMENTED
        );
    }

    #[test]
    fn composition_rejects_duplicate_ownership_and_recipe_drift() {
        let duplicate_action = CompositeSurface::try_new(vec![
            Box::new(TestExtension::new("alpha", "set", Some("/alpha"))),
            Box::new(TestExtension::new("beta", "set", Some("/beta"))),
        ])
        .err()
        .unwrap()
        .to_string();
        assert!(duplicate_action.contains("owned by more than one"));

        let duplicate_route = CompositeSurface::try_new(vec![
            Box::new(TestExtension::new("alpha", "set_alpha", Some("/same"))),
            Box::new(TestExtension::new("beta", "set_beta", Some("/same"))),
        ])
        .err()
        .unwrap()
        .to_string();
        assert!(duplicate_route.contains("owned by more than one"));

        let foreign_action_route = CompositeSurface::try_new(vec![Box::new(
            TestExtension::new("alpha", "set_alpha", None)
                .action_route("/write", "set_someone_else"),
        )])
        .err()
        .unwrap()
        .to_string();
        assert!(foreign_action_route.contains("does not own"));

        let duplicate_action_route = CompositeSurface::try_new(vec![
            Box::new(
                TestExtension::new("alpha", "set_alpha", None).action_route("/write", "set_alpha"),
            ),
            Box::new(
                TestExtension::new("beta", "set_beta", None).action_route("/write", "set_beta"),
            ),
        ])
        .err()
        .unwrap()
        .to_string();
        assert!(duplicate_action_route.contains("owned by more than one"));

        let recipe = recipe(
            r#"
schema_version = 1
[app]
id = "test-app"
profile = "web"
[[extensions]]
id = "alpha"
version = "2.0.0"
capabilities = ["dom"]
"#,
        );
        let drift = CompositeSurface::from_recipe(
            &recipe,
            vec![Box::new(TestExtension::new("alpha", "set_alpha", None))],
        )
        .err()
        .unwrap()
        .to_string();
        assert!(drift.contains("pins alpha@2.0.0"));
    }

    #[test]
    fn composition_reserves_the_runtime_mcp_route() {
        let error = CompositeSurface::try_new(vec![Box::new(TestExtension::new(
            "alpha",
            "set_alpha",
            Some("/mcp"),
        ))])
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("collides with a core runtime route"));
    }

    #[test]
    fn recipe_is_strict_and_requires_an_enabled_extension() {
        let unknown = toml::from_str::<AppRecipe>(
            r#"
schema_version = 1
surprise = true
[app]
id = "test-app"
profile = "web"
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(unknown.contains("unknown field"));

        let disabled: AppRecipe = toml::from_str(
            r#"
schema_version = 1
[app]
id = "test-app"
profile = "web"
[[extensions]]
id = "alpha"
version = "1.0.0"
enabled = false
"#,
        )
        .unwrap();
        assert!(disabled
            .validate()
            .unwrap_err()
            .to_string()
            .contains("at least one"));
    }
}
