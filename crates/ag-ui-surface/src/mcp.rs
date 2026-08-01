//! Runtime-owned MCP projection for ACP providers.
//!
//! The effective [`Surface::tools`](crate::Surface::tools) vocabulary is the
//! only schema source. Composed applications therefore inherit the exact
//! recipe-selected extension set, while direct Surfaces use the same path.

use std::collections::HashSet;

use axum::http::{header, HeaderMap};
use base64::Engine as _;
use serde_json::{json, Value};

use crate::runtime_state::RuntimeState;
use crate::{turn_loop, ActionAudience, ActionDef, Caller, Surface};

/// The MCP revision this implementation negotiates. Newer clients may
/// negotiate down to it; older `2025-03-26` clients are not advertised because
/// that revision requires JSON-RPC batch handling, which this server omits.
pub(crate) const PROTOCOL_VERSION: &str = "2025-06-18";
const MISSING_HEADER_FALLBACK_VERSION: &str = "2025-03-26";
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[PROTOCOL_VERSION];
const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";
/// Assigned by this server at `initialize` and echoed by the client on every
/// later request. Without it every MCP caller is the same anonymous agent,
/// which is why exactly one agent byline existed before this.
pub(crate) const SESSION_ID_HEADER: &str = "mcp-session-id";

/// What an MCP client says about itself at `initialize`.
///
/// Every field here is the client's own **claim**. It is a proposed display
/// name, never an identity: the participant id is minted by this host, because
/// a caller that could choose its own id could impersonate one already in the
/// room. Same reasoning as `board_compose` minting ids from the revision
/// rather than accepting one from the browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClientIdentity {
    pub name: String,
    pub version: Option<String>,
    pub title: Option<String>,
}

/// The identity an `initialize` message proposes, if this is one.
///
/// Returns `None` for every other method, so a caller can use it to detect an
/// attach without re-parsing the envelope.
pub(crate) fn client_identity(message: &Value) -> Option<ClientIdentity> {
    if message.get("method").and_then(Value::as_str) != Some("initialize") {
        return None;
    }
    let info = message.get("params")?.get("clientInfo")?;
    let text = |key: &str| {
        info.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    // A client that omits `clientInfo.name` is still allowed to attach; it
    // simply arrives unnamed rather than borrowing someone else's byline.
    Some(ClientIdentity {
        name: text("name").unwrap_or_else(|| "unidentified agent".to_string()),
        version: text("version"),
        title: text("title"),
    })
}

/// The session an already-attached client is presenting.
pub(crate) fn session_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get(SESSION_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub mcp_apps_visibility: &'static [&'static str],
    pub include_state_snapshot: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Catalog {
    tools: Vec<ToolSchema>,
}

impl Catalog {
    pub(crate) fn from_actions(actions: &[ActionDef]) -> Result<Self, String> {
        let mut names = HashSet::new();
        let mut tools = Vec::new();
        for action in actions {
            let Some(mcp_apps_visibility) = mcp_apps_visibility(action.audience) else {
                // Human-only actions stay absent rather than being disclosed
                // as app-only MCP tools. The browser has its own manifest.
                continue;
            };
            if action.name.trim().is_empty() {
                return Err("agent-visible action has an empty name".to_string());
            }
            if !names.insert(action.name.clone()) {
                return Err(format!(
                    "agent-visible action {:?} is declared more than once",
                    action.name
                ));
            }
            if !action.parameters.is_object()
                || action.parameters.get("type").and_then(Value::as_str) != Some("object")
            {
                return Err(format!(
                    "agent-visible action {:?} input schema must declare type=object",
                    action.name
                ));
            }
            tools.push(ToolSchema {
                name: action.name.clone(),
                description: action.description.clone(),
                input_schema: action.parameters.clone(),
                mcp_apps_visibility,
                include_state_snapshot: action.include_state_snapshot,
            });
        }
        Ok(Self { tools })
    }

    fn get(&self, name: &str) -> Option<&ToolSchema> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    pub(crate) fn wire_tools(&self) -> Vec<Value> {
        self.tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                    "_meta": {
                        "ui": {
                            "visibility": tool.mcp_apps_visibility,
                        }
                    },
                })
            })
            .collect()
    }
}

/// Project the already-agent-visible catalog into MCP Apps audience metadata.
///
/// MCP Apps permits app-only entries, but this runtime deliberately omits
/// human-only actions from MCP altogether so their existence is not disclosed
/// to the model-facing connection.
fn mcp_apps_visibility(audience: ActionAudience) -> Option<&'static [&'static str]> {
    match audience {
        ActionAudience::Human => None,
        ActionAudience::Agent => Some(&["model"]),
        ActionAudience::Both => Some(&["model", "app"]),
    }
}

pub(crate) enum Reply {
    Accepted,
    Json(Value),
}

/// Handle one MCP JSON-RPC request or notification. HTTP concurrency is the
/// concurrency model: a long `EmitAndAwait` call suspends only its own request,
/// so another named decision can remain in flight independently.
pub(crate) async fn handle(
    catalog: &Catalog,
    rt: &RuntimeState,
    surface: &dyn Surface,
    message: Value,
) -> Reply {
    let Some(object) = message.as_object() else {
        return Reply::Json(error(Value::Null, -32600, "invalid JSON-RPC request"));
    };
    let id = object.get("id").cloned();
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Reply::Json(error(
            id.unwrap_or(Value::Null),
            -32600,
            "invalid JSON-RPC version",
        ));
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Reply::Json(error(
            id.unwrap_or(Value::Null),
            -32600,
            "JSON-RPC method must be a string",
        ));
    };

    // JSON-RPC notifications never receive a JSON-RPC body. The initialized
    // notification is the only one this stateless tool server needs today.
    let Some(id) = id else {
        return Reply::Accepted;
    };

    let result = match method {
        "initialize" => {
            let requested = object
                .get("params")
                .and_then(|params| params.get("protocolVersion"))
                .and_then(Value::as_str);
            match requested {
                Some(requested) => {
                    let version = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
                        requested
                    } else {
                        PROTOCOL_VERSION
                    };
                    *rt.mcp_protocol_version.lock() = Some(version.to_string());
                    Ok(json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": {
                            "name": "ag-ui-surface",
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    }))
                }
                None => Err((
                    -32602,
                    "initialize.params.protocolVersion must be a string".to_string(),
                )),
            }
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": catalog.wire_tools() })),
        "tools/call" => Ok(call_tool(catalog, rt, surface, object.get("params")).await),
        _ => Err((-32601, format!("method not found: {method}"))),
    };

    Reply::Json(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => error(id, code, &message),
    })
}

async fn call_tool(
    catalog: &Catalog,
    rt: &RuntimeState,
    surface: &dyn Surface,
    params: Option<&Value>,
) -> Value {
    let name = params
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let args = params
        .and_then(|params| params.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));

    let Some(schema) = catalog.get(name) else {
        return tool_error(format!("action {name:?} is not visible to this agent"));
    };

    // Re-check the executable definition, not just the serialized catalog.
    // This fails closed if a future refactor ever lets those two drift.
    if !surface
        .tools()
        .iter()
        .any(|action| action.name == name && action.audience.agent())
    {
        return tool_error(format!("action {name:?} is not enabled"));
    }

    // MCP requires server-side input validation. Do not duplicate it in this
    // transport adapter: dispatch_tool validates the startup-compiled ActionDef
    // schema before apply, exactly as it does for OpenAI and human calls — and
    // re-checks the audience against `Caller::Agent`, which is now the
    // authoritative gate rather than the two checks above.
    let (text, _was_query, ok) =
        turn_loop::dispatch_tool(rt, surface, Caller::Agent, name, &args).await;
    let mut content = vec![json!({ "type": "text", "text": text })];
    if ok && schema.include_state_snapshot {
        match surface.state().snapshot_png().await {
            Ok(Some(png)) => content.push(json!({
                "type": "image",
                "data": base64::engine::general_purpose::STANDARD.encode(png),
                "mimeType": "image/png"
            })),
            Ok(None) => {}
            Err(error) => {
                return tool_error(format!("state snapshot failed after `{name}`: {error}"));
            }
        }
    }
    json!({ "content": content, "isError": !ok })
}

fn tool_error(message: String) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true
    })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

pub(crate) fn authorized(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {token}"))
}

/// Browsers send `Origin`; ACP's server-side HTTP client normally does not.
/// If present, accept only loopback origins to prevent DNS rebinding into a
/// locally-bound MCP action surface.
pub(crate) fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    [
        "http://localhost",
        "https://localhost",
        "http://127.0.0.1",
        "https://127.0.0.1",
        "http://[::1]",
        "https://[::1]",
    ]
    .iter()
    .any(|prefix| {
        origin
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(':'))
    })
}

/// Enforce the Streamable HTTP protocol-version header after initialization
/// and bind it to the version selected for this runtime's active ACP session.
pub(crate) fn protocol_version_allowed(
    headers: &HeaderMap,
    message: &Value,
    rt: &RuntimeState,
) -> Result<(), String> {
    if message.get("method").and_then(Value::as_str) == Some("initialize") {
        return Ok(());
    }
    let version = match headers.get(PROTOCOL_VERSION_HEADER) {
        Some(value) => value
            .to_str()
            .map_err(|_| "invalid MCP-Protocol-Version header".to_string())?,
        // The transport compatibility rule says a stateless server should
        // interpret a missing header as 2025-03-26. This runtime deliberately
        // does not support that batch-capable revision, so the common version
        // check below rejects it rather than silently overclaiming support.
        None => MISSING_HEADER_FALLBACK_VERSION,
    };
    if !SUPPORTED_PROTOCOL_VERSIONS.contains(&version) {
        return Err(format!(
            "unsupported MCP-Protocol-Version {version:?}; supported versions: {}",
            SUPPORTED_PROTOCOL_VERSIONS.join(", ")
        ));
    }
    let negotiated = rt
        .mcp_protocol_version
        .lock()
        .clone()
        .ok_or_else(|| "MCP session has not been initialized".to_string())?;
    if version != negotiated {
        return Err(format!(
            "MCP-Protocol-Version {version:?} does not match negotiated version {negotiated:?}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod identity_tests {
    use super::{client_identity, session_id};
    use axum::http::HeaderMap;
    use serde_json::json;

    #[test]
    fn only_initialize_announces_an_identity() {
        assert!(client_identity(&json!({
            "method": "tools/list",
            "params": { "clientInfo": { "name": "claude" } }
        }))
        .is_none());
    }

    #[test]
    fn initialize_carries_the_clients_proposed_name() {
        let identity = client_identity(&json!({
            "method": "initialize",
            "params": { "clientInfo": { "name": "claude", "version": "0.1" } }
        }))
        .expect("initialize announces an identity");
        assert_eq!(identity.name, "claude");
        assert_eq!(identity.version.as_deref(), Some("0.1"));
    }

    #[test]
    fn a_client_that_names_itself_nothing_is_unidentified_not_anonymous() {
        // It still attaches; it simply does not get to borrow a byline by
        // omitting one.
        let identity = client_identity(&json!({
            "method": "initialize",
            "params": { "clientInfo": { "name": "   " } }
        }))
        .expect("initialize announces an identity");
        assert_eq!(identity.name, "unidentified agent");
    }

    #[test]
    fn initialize_without_client_info_announces_nothing() {
        assert!(client_identity(&json!({ "method": "initialize", "params": {} })).is_none());
    }

    #[test]
    fn a_blank_session_header_is_no_session() {
        let mut headers = HeaderMap::new();
        headers.insert("mcp-session-id", "   ".parse().unwrap());
        assert_eq!(session_id(&headers), None);
        headers.insert("mcp-session-id", "abc123".parse().unwrap());
        assert_eq!(session_id(&headers).as_deref(), Some("abc123"));
    }
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;
    use std::sync::Arc;

    use crate::auth::AuthStore;
    use crate::turn_loop::openai::ByokConfig;
    use ag_ui_core::event::BaseEvent;

    use crate::{
        ActionAudience, CustomEvent, Effect, ReplyKind, StateBacking, StateSnapshot, SurfaceState,
        ToolDef,
    };

    use super::*;

    struct TestState {
        values: Mutex<Vec<String>>,
        fail_png: std::sync::atomic::AtomicBool,
    }

    impl TestState {
        fn new() -> Self {
            Self {
                values: Mutex::new(Vec::new()),
                fail_png: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }

    impl SurfaceState for TestState {
        fn backing(&self) -> StateBacking {
            StateBacking::Ephemeral
        }

        fn describe(&self) -> Result<String, String> {
            Ok(self.values.lock().join(","))
        }

        fn snapshot_png(&self) -> crate::SnapshotPngFuture<'_> {
            Box::pin(async {
                if self.fail_png.load(std::sync::atomic::Ordering::Relaxed) {
                    Err("renderer unavailable".to_string())
                } else {
                    Ok(Some(vec![1, 2, 3]))
                }
            })
        }

        fn snapshot(&self) -> Result<StateSnapshot, String> {
            Ok(StateSnapshot {
                backing: StateBacking::Ephemeral,
                body: json!({ "values": *self.values.lock() }),
                chrome: None,
            })
        }
    }

    struct TestSurface {
        state: Arc<TestState>,
        actions: Vec<ActionDef>,
    }

    impl Surface for TestSurface {
        fn state(&self) -> &dyn SurfaceState {
            self.state.as_ref()
        }

        fn tools(&self) -> &[ToolDef] {
            &self.actions
        }

        fn client_modules(&self) -> Vec<crate::ClientModule> {
            vec![crate::ClientModule::host("test", "1", "test")]
        }
    }

    fn runtime(actions: &[ActionDef]) -> Arc<RuntimeState> {
        let auth_path = std::env::temp_dir().join(format!(
            "ag-ui-mcp-auth-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let (ws_tx, _) = tokio::sync::broadcast::channel(8);
        let (sse_tx, _) = tokio::sync::broadcast::channel(8);
        let rt = RuntimeState::new(
            ws_tx,
            sse_tx,
            Arc::new(Mutex::new(std::collections::VecDeque::new())),
            Vec::new(),
            Arc::new(AuthStore::open(auth_path).expect("temporary auth store should open")),
            "test".to_string(),
            ByokConfig::default(),
            false,
            0,
        )
        .0;
        rt.install_action_schemas(actions).unwrap();
        rt
    }

    fn surface() -> TestSurface {
        let state = Arc::new(TestState::new());
        let both = ToolDef::new(
            "add",
            "Add one value.",
            json!({ "type": "object", "properties": { "value": { "type": "string" } } }),
            {
                let state = state.clone();
                move |args| {
                    let state = state.clone();
                    let value = args
                        .get("value")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    Effect::Mutate(Box::new(move |_surface| {
                        state.values.lock().push(value);
                        Ok(None)
                    }))
                }
            },
        );
        let human = ToolDef::new(
            "human_reset",
            "Human-only reset.",
            json!({ "type": "object", "properties": {} }),
            |_args| Effect::Mutate(Box::new(|_surface| Ok(None))),
        )
        .audience(ActionAudience::Human);
        let agent = ToolDef::new(
            "inspect",
            "Inspect values.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            {
                let state = state.clone();
                move |_args| {
                    let state = state.clone();
                    Effect::Query(Box::new(move |_surface| Ok(state.values.lock().join(","))))
                }
            },
        )
        .agent_only()
        .with_state_snapshot();
        let reject = ToolDef::new(
            "reject",
            "Always reject.",
            json!({ "type": "object", "properties": {} }),
            |_args| Effect::Reject("not allowed".to_string()),
        );
        TestSurface {
            state,
            actions: vec![both, human, agent, reject],
        }
    }

    #[tokio::test]
    async fn the_dispatcher_refuses_each_caller_the_other_side_actions() {
        let surface = surface();
        let rt = runtime(surface.tools());

        // The regression this exists for: `POST /surface/action` reached
        // `dispatch_tool` without any audience check, so an `agent_only`
        // action ran from the browser even when the extension manifest
        // advertised no human actions at all.
        let (text, _, ok) =
            turn_loop::dispatch_tool(&rt, &surface, Caller::Human, "inspect", &json!({})).await;
        assert!(!ok, "the human must not reach an agent-only action");
        assert!(text.contains("not available to the human"), "{text}");

        let (text, _, ok) =
            turn_loop::dispatch_tool(&rt, &surface, Caller::Agent, "human_reset", &json!({})).await;
        assert!(!ok, "the agent must not reach a human-only action");
        assert!(text.contains("not available to this agent"), "{text}");

        // Each side keeps its own, and `Both` stays open to everyone.
        for (caller, name) in [
            (Caller::Human, "human_reset"),
            (Caller::Agent, "inspect"),
            (Caller::Human, "add"),
            (Caller::Agent, "add"),
        ] {
            let (text, _, ok) =
                turn_loop::dispatch_tool(&rt, &surface, caller, name, &json!({})).await;
            assert!(ok, "{caller:?} must still reach {name:?}: {text}");
        }
    }

    #[tokio::test]
    async fn a_forbidden_action_is_refused_before_its_arguments_are_read() {
        let surface = surface();
        let rt = runtime(surface.tools());

        // `inspect` declares additionalProperties:false, so these arguments
        // would fail schema validation. The refusal must still be about who is
        // calling — otherwise the shape of the error reveals whether a given
        // action exists and what it accepts.
        let (text, _, ok) = turn_loop::dispatch_tool(
            &rt,
            &surface,
            Caller::Human,
            "inspect",
            &json!({ "unexpected": 1 }),
        )
        .await;
        assert!(!ok);
        assert!(
            text.contains("not available to the human"),
            "audience must be decided before the payload: {text}"
        );
    }

    #[test]
    fn caller_and_audience_agree_on_every_combination() {
        use ActionAudience::{Agent, Both, Human};
        assert!(Caller::Human.may_call(Human));
        assert!(Caller::Human.may_call(Both));
        assert!(!Caller::Human.may_call(Agent));
        assert!(Caller::Agent.may_call(Agent));
        assert!(Caller::Agent.may_call(Both));
        assert!(!Caller::Agent.may_call(Human));
    }

    #[tokio::test]
    async fn requests_without_exact_jsonrpc_2_are_rejected_before_dispatch() {
        let surface = surface();
        let catalog = Catalog::from_actions(surface.tools()).unwrap();
        let rt = runtime(surface.tools());
        for message in [
            json!({ "id": 1, "method": "tools/list" }),
            json!({ "jsonrpc": "1.0", "id": 2, "method": "tools/list" }),
            json!({ "jsonrpc": 2, "id": 3, "method": "tools/list" }),
        ] {
            let Reply::Json(reply) = handle(&catalog, &rt, &surface, message).await else {
                panic!("invalid request must receive JSON-RPC error")
            };
            assert_eq!(reply["jsonrpc"], "2.0");
            assert_eq!(reply["error"]["code"], -32600);
            assert!(reply.get("result").is_none());
        }
    }

    #[test]
    fn catalog_is_an_exact_agent_visible_projection() {
        let surface = surface();
        let catalog = Catalog::from_actions(surface.tools()).unwrap();
        let tools = catalog.wire_tools();
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["add", "inspect", "reject"]
        );
        assert_eq!(tools[0]["description"], "Add one value.");
        assert_eq!(tools[0]["inputSchema"], surface.tools()[0].parameters);
        assert!(tools.iter().all(|tool| tool["name"] != "human_reset"));
    }

    async fn tools_list_response(surface: &TestSurface) -> Value {
        let catalog = Catalog::from_actions(surface.tools()).unwrap();
        let rt = runtime(surface.tools());
        let Reply::Json(reply) = handle(
            &catalog,
            &rt,
            surface,
            json!({ "jsonrpc": "2.0", "id": 41, "method": "tools/list" }),
        )
        .await
        else {
            panic!("tools/list request must reply")
        };
        reply
    }

    #[tokio::test]
    async fn tools_list_maps_action_audiences_to_mcp_apps_visibility() {
        let surface = surface();
        let reply = tools_list_response(&surface).await;
        println!(
            "{}",
            serde_json::to_string_pretty(&reply)
                .expect("tools/list response must serialize for inspection")
        );

        let tools = reply["result"]["tools"]
            .as_array()
            .expect("tools/list result must contain a tools array");
        assert_eq!(tools[0]["name"], "add");
        assert_eq!(
            tools[0]["_meta"]["ui"]["visibility"],
            json!(["model", "app"])
        );
        assert_eq!(tools[1]["name"], "inspect");
        assert_eq!(tools[1]["_meta"]["ui"]["visibility"], json!(["model"]));
        assert_eq!(tools[2]["name"], "reject");
        assert_eq!(
            tools[2]["_meta"]["ui"]["visibility"],
            json!(["model", "app"])
        );
    }

    #[tokio::test]
    async fn tools_list_never_discloses_human_only_actions() {
        let surface = surface();
        let reply = tools_list_response(&surface).await;
        let tools = reply["result"]["tools"]
            .as_array()
            .expect("tools/list result must contain a tools array");

        assert!(
            tools
                .iter()
                .all(|tool| tool["name"].as_str() != Some("human_reset")),
            "human-only actions must remain absent, not app-only MCP entries: {reply}"
        );
    }

    #[tokio::test]
    async fn calls_use_real_dispatch_reject_hidden_actions_and_attach_opt_in_pixels() {
        let surface = surface();
        let catalog = Catalog::from_actions(surface.tools()).unwrap();
        let rt = runtime(surface.tools());

        let invalid = handle(
            &catalog,
            &rt,
            &surface,
            json!({ "jsonrpc": "2.0", "id": 0, "method": "tools/call", "params": { "name": "add", "arguments": { "value": 7 } } }),
        )
        .await;
        let Reply::Json(invalid) = invalid else {
            panic!("request must reply")
        };
        assert_eq!(invalid["result"]["isError"], true);
        assert!(invalid["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("invalid arguments"));
        assert!(surface.state.values.lock().is_empty());

        let add = handle(
            &catalog,
            &rt,
            &surface,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "add", "arguments": { "value": "real" } } }),
        )
        .await;
        let Reply::Json(add) = add else {
            panic!("request must reply")
        };
        assert_eq!(add["result"]["isError"], false);
        assert_eq!(
            surface
                .state
                .describe()
                .expect("read test surface description"),
            "real"
        );

        let inspect = handle(
            &catalog,
            &rt,
            &surface,
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "inspect", "arguments": {} } }),
        )
        .await;
        let Reply::Json(inspect) = inspect else {
            panic!("request must reply")
        };
        assert_eq!(inspect["result"]["content"][0]["text"], "real");
        assert_eq!(inspect["result"]["content"][1]["type"], "image");
        assert_eq!(inspect["result"]["content"][1]["data"], "AQID");

        surface
            .state
            .fail_png
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let failed_snapshot = handle(
            &catalog,
            &rt,
            &surface,
            json!({ "jsonrpc": "2.0", "id": 20, "method": "tools/call", "params": { "name": "inspect", "arguments": {} } }),
        )
        .await;
        let Reply::Json(failed_snapshot) = failed_snapshot else {
            panic!("request must reply")
        };
        assert_eq!(failed_snapshot["result"]["isError"], true);
        assert!(failed_snapshot["result"]["content"][0]["text"]
            .as_str()
            .expect("snapshot error text")
            .contains("state snapshot failed after `inspect`: renderer unavailable"));

        for (id, name) in [(3, "human_reset"), (4, "missing"), (5, "reject")] {
            let reply = handle(
                &catalog,
                &rt,
                &surface,
                json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": {} } }),
            )
            .await;
            let Reply::Json(reply) = reply else {
                panic!("request must reply")
            };
            assert_eq!(reply["result"]["isError"], true, "{name}");
        }
    }

    #[tokio::test]
    async fn concurrent_http_calls_keep_emit_and_await_decisions_independent() {
        let state = Arc::new(TestState::new());
        let wait = ToolDef::new(
            "wait",
            "Wait for one keyed human reply.",
            json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }),
            |args| {
                let id = args
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let reply_id = id.clone();
                Effect::EmitAndAwait {
                    id,
                    event: CustomEvent {
                        base: BaseEvent::default(),
                        name: "test.wait".to_string(),
                        value: json!({ "id": reply_id.clone() }),
                    },
                    reply_kind: ReplyKind::Any,
                    on_reply: Box::new(move |reply| {
                        Ok(format!(
                            "{reply_id}:{}",
                            reply.get("value").and_then(Value::as_str).unwrap_or("")
                        ))
                    }),
                }
            },
        );
        let surface = TestSurface {
            state,
            actions: vec![wait],
        };
        let catalog = Catalog::from_actions(surface.tools()).unwrap();
        let rt = runtime(surface.tools());

        let first = handle(
            &catalog,
            &rt,
            &surface,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "wait", "arguments": { "id": "a" } } }),
        );
        let second = handle(
            &catalog,
            &rt,
            &surface,
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "wait", "arguments": { "id": "b" } } }),
        );
        let resolve_out_of_order = async {
            loop {
                if rt.decision.lock().len() == 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
            let mut b = rt.decision.lock().remove("b").unwrap();
            let result = (b.on_reply.take().unwrap())(json!({ "value": "second" }));
            b.reply_tx.take().unwrap().send(result).unwrap();
            let mut a = rt.decision.lock().remove("a").unwrap();
            let result = (a.on_reply.take().unwrap())(json!({ "value": "first" }));
            a.reply_tx.take().unwrap().send(result).unwrap();
        };

        let (first, second, ()) = tokio::join!(first, second, resolve_out_of_order);
        let Reply::Json(first) = first else {
            panic!("request must reply")
        };
        let Reply::Json(second) = second else {
            panic!("request must reply")
        };
        assert_eq!(first["result"]["content"][0]["text"], "a:first");
        assert_eq!(second["result"]["content"][0]["text"], "b:second");
    }

    #[tokio::test]
    async fn duplicate_decision_id_is_rejected_without_cancelling_original() {
        let state = Arc::new(TestState::new());
        let wait = ToolDef::new(
            "wait",
            "Wait for one keyed human reply.",
            json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }),
            |args| {
                let id = args
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let reply_id = id.clone();
                Effect::EmitAndAwait {
                    id,
                    event: CustomEvent {
                        base: BaseEvent::default(),
                        name: "test.wait".to_string(),
                        value: json!({ "id": reply_id.clone() }),
                    },
                    reply_kind: ReplyKind::Any,
                    on_reply: Box::new(move |reply| {
                        Ok(format!(
                            "{reply_id}:{}",
                            reply.get("value").and_then(Value::as_str).unwrap_or("")
                        ))
                    }),
                }
            },
        );
        let surface = TestSurface {
            state,
            actions: vec![wait],
        };
        let catalog = Catalog::from_actions(surface.tools()).unwrap();
        let rt = runtime(surface.tools());

        let original = handle(
            &catalog,
            &rt,
            &surface,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "wait", "arguments": { "id": "same" } } }),
        );
        let reject_duplicate_then_resolve = async {
            loop {
                if rt.decision.lock().contains_key("same") {
                    break;
                }
                tokio::task::yield_now().await;
            }

            let duplicate = handle(
                &catalog,
                &rt,
                &surface,
                json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "wait", "arguments": { "id": "same" } } }),
            )
            .await;
            assert_eq!(rt.decision.lock().len(), 1);

            let mut pending = rt.decision.lock().remove("same").unwrap();
            let result = (pending.on_reply.take().unwrap())(json!({ "value": "original" }));
            pending.reply_tx.take().unwrap().send(result).unwrap();
            duplicate
        };

        let (original, duplicate) = tokio::join!(original, reject_duplicate_then_resolve);
        let Reply::Json(original) = original else {
            panic!("request must reply")
        };
        let Reply::Json(duplicate) = duplicate else {
            panic!("request must reply")
        };
        assert_eq!(original["result"]["isError"], false);
        assert_eq!(original["result"]["content"][0]["text"], "same:original");
        assert_eq!(duplicate["result"]["isError"], true);
        assert!(duplicate["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("already pending"));
    }

    #[test]
    fn catalog_rejects_non_object_agent_schemas() {
        let invalid = ToolDef::new(
            "invalid",
            "Invalid schema.",
            json!({ "type": "string" }),
            |_args| Effect::Query(Box::new(|_state| Ok("no".to_string()))),
        );
        assert!(Catalog::from_actions(&[invalid])
            .unwrap_err()
            .contains("type=object"));
    }

    #[test]
    fn origin_and_bearer_checks_fail_closed() {
        let mut headers = HeaderMap::new();
        assert!(origin_allowed(&headers));
        assert!(!authorized(&headers, "secret"));
        headers.insert(header::AUTHORIZATION, "Bearer secret".parse().unwrap());
        assert!(authorized(&headers, "secret"));
        headers.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        assert!(!origin_allowed(&headers));
        headers.insert(header::ORIGIN, "http://127.0.0.1:8091".parse().unwrap());
        assert!(origin_allowed(&headers));
    }

    #[tokio::test]
    async fn protocol_versions_are_negotiated_and_subsequent_headers_are_checked() {
        let surface = surface();
        let catalog = Catalog::from_actions(surface.tools()).unwrap();
        let rt = runtime(surface.tools());

        for (requested, expected) in [
            ("2025-06-18", "2025-06-18"),
            ("2025-11-25", PROTOCOL_VERSION),
            ("2025-03-26", PROTOCOL_VERSION),
        ] {
            let reply = handle(
                &catalog,
                &rt,
                &surface,
                json!({
                    "jsonrpc": "2.0",
                    "id": requested,
                    "method": "initialize",
                    "params": { "protocolVersion": requested }
                }),
            )
            .await;
            let Reply::Json(reply) = reply else {
                panic!("initialize must reply")
            };
            assert_eq!(reply["result"]["protocolVersion"], expected);
            assert_eq!(
                rt.mcp_protocol_version.lock().as_deref(),
                Some(PROTOCOL_VERSION)
            );
        }

        let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
        let mut headers = HeaderMap::new();
        assert!(protocol_version_allowed(&headers, &request, &rt).is_err());
        headers.insert(PROTOCOL_VERSION_HEADER, "2025-06-18".parse().unwrap());
        assert!(protocol_version_allowed(&headers, &request, &rt).is_ok());
        headers.insert(PROTOCOL_VERSION_HEADER, "not-a-version".parse().unwrap());
        assert!(protocol_version_allowed(&headers, &request, &rt).is_err());

        // Initialization negotiates the version in its body, not this header.
        let initialize = json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize" });
        assert!(protocol_version_allowed(&headers, &initialize, &rt).is_ok());

        let missing = handle(
            &catalog,
            &rt,
            &surface,
            json!({ "jsonrpc": "2.0", "id": 3, "method": "initialize", "params": {} }),
        )
        .await;
        let Reply::Json(missing) = missing else {
            panic!("invalid initialize must reply")
        };
        assert_eq!(missing["error"]["code"], -32602);
    }
}
