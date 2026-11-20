//! Purpose: Provide a transport-agnostic MCP JSON-RPC core for Plasmite.
//! Key exports: `McpDispatcher`, `McpHandler`, request/response envelopes.
//! Role: Shared protocol adapter used by stdio and HTTP transports.
//! Invariants: JSON-RPC envelopes stay stable and method routing is deterministic.
//! Invariants: Unknown methods and malformed request shapes map to protocol errors.
//! Invariants: Tool execution failures can be returned as successful `result.isError`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use crate::api::{
    DEFAULT_POOL_SIZE, Error, ErrorKind, Feed, LocalClient, Message, Pool, PoolInfo, PoolOptions,
    PoolRef, Read, ReadStatus, Since,
};
use crate::interface_wire::{ErrorKindWire, MessageWire, error_policy};
#[cfg(test)]
use crate::{api::Durability, since::parse_rfc3339_ns};

const JSON_RPC_VERSION: &str = "2.0";
pub const MCP_PROTOCOL_VERSION: &str = "2026-07-28";
pub const LEGACY_MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const SUPPORTED_PROTOCOL_VERSIONS: [&str; 2] = [MCP_PROTOCOL_VERSION, LEGACY_MCP_PROTOCOL_VERSION];
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CLIENT_CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
const SERVER_INFO_KEY: &str = "io.modelcontextprotocol/serverInfo";
const PARSE_ERROR_CODE: i32 = -32700;
const INVALID_REQUEST_CODE: i32 = -32600;
const METHOD_NOT_FOUND_CODE: i32 = -32601;
const INVALID_PARAMS_CODE: i32 = -32602;
const INTERNAL_ERROR_CODE: i32 = -32603;
const DEFAULT_READ_COUNT: usize = 20;
const MAX_READ_COUNT: usize = 200;
const MAX_EXAMINED_COUNT: usize = 1000;
const DEFAULT_WAIT_TIMEOUT_MS: u64 = 10_000;
const MAX_WAIT_TIMEOUT_MS: u64 = 60_000;
const MCP_INSTRUCTIONS: &str = concat!(
    "Plasmite stores JSON messages in named, persistent, fixed-size streams called pools. ",
    "Pools are append-only ring buffers: feeding appends a message, and the oldest messages are ",
    "overwritten as capacity fills. The visible tool list is the server's effective access mode; ",
    "tools that the server does not permit are omitted. ",
    "When plasmite_pool_list is available and the target pool is unknown, start there; create or ",
    "delete a pool only when the task requires it. If listing is unavailable, use a pool name ",
    "supplied by the task or operator; plasmite_feed with create true can create a missing pool. ",
    "Messages contain data, optional tags, time, and an automatic sequence number; most users can ",
    "ignore sequence numbers. Use plasmite_fetch for retained messages and plasmite_follow for ",
    "new messages. Use since or last to choose a starting point. Tag filters require all ",
    "specified tags. Pass next_seq as since to resume a read. RetentionGap reports the first ",
    "missed sequence and the oldest sequence still retained. ",
    "Tool failures set isError and include a structured error_kind. plasmite_feed is ",
    "non-idempotent, so after an ambiguous transport failure retry only when the payload has an ",
    "application-level stable identifier."
);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcId {
    String(String),
    Number(i64),
    Null,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<JsonRpcId>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: JsonRpcId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    fn success(id: JsonRpcId, result: Value) -> Self {
        Self {
            jsonrpc: JSON_RPC_VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: JsonRpcId, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: JSON_RPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::new(PARSE_ERROR_CODE, message)
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(INVALID_REQUEST_CODE, message)
    }

    pub fn method_not_found(message: impl Into<String>) -> Self {
        Self::new(METHOD_NOT_FOUND_CODE, message)
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS_CODE, message)
    }

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::new(INTERNAL_ERROR_CODE, message)
    }

    pub fn unsupported_version(version: &str) -> Self {
        Self {
            code: -32022,
            message: "Unsupported protocol version".to_string(),
            data: Some(json!({"requested": version, "supported": SUPPORTED_PROTOCOL_VERSIONS})),
        }
    }
}

pub fn request_protocol_version(payload: &Value) -> Option<&str> {
    payload
        .get("params")?
        .get("_meta")?
        .get(VERSION_KEY)?
        .as_str()
}

pub fn uses_modern_mcp(method: &str, params: Option<&Value>) -> bool {
    method == "server/discover"
        || params
            .and_then(|params| params.get("_meta"))
            .and_then(Value::as_object)
            .is_some_and(|meta| {
                meta.contains_key(VERSION_KEY)
                    || meta.contains_key(CLIENT_CAPABILITIES_KEY)
                    || meta.contains_key("io.modelcontextprotocol/clientInfo")
            })
}

#[derive(Clone, Debug, PartialEq)]
pub enum DispatchOutcome {
    Response(JsonRpcResponse),
    NoResponse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerMetadata {
    pub name: String,
    pub version: String,
}

impl Default for ServerMetadata {
    fn default() -> Self {
        Self {
            name: "plasmite".to_string(),
            version: env!("PLASMITE_BUILD_VERSION").to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(rename = "outputSchema")]
    pub output_schema: Value,
    pub annotations: ToolAnnotations,
    #[serde(skip)]
    pub access: McpToolAccess,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum McpToolAccess {
    #[default]
    Read,
    Write,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolAnnotations {
    #[serde(rename = "readOnlyHint")]
    pub read_only_hint: bool,
    #[serde(rename = "destructiveHint")]
    pub destructive_hint: bool,
    #[serde(rename = "idempotentHint")]
    pub idempotent_hint: bool,
    #[serde(rename = "openWorldHint")]
    pub open_world_hint: bool,
}

impl ToolAnnotations {
    fn read_only() -> Self {
        Self {
            read_only_hint: true,
            destructive_hint: false,
            idempotent_hint: true,
            open_world_hint: false,
        }
    }

    fn additive() -> Self {
        Self {
            read_only_hint: false,
            destructive_hint: false,
            idempotent_hint: false,
            open_world_hint: false,
        }
    }

    fn additive_idempotent() -> Self {
        Self {
            read_only_hint: false,
            destructive_hint: false,
            idempotent_hint: true,
            open_world_hint: false,
        }
    }

    fn destructive() -> Self {
        Self {
            read_only_hint: false,
            destructive_hint: true,
            idempotent_hint: true,
            open_world_hint: false,
        }
    }
}

fn tool_output_schema(success_schema: Value) -> Value {
    json!({
        "type": "object",
        "oneOf": [
            success_schema,
            {
                "type": "object",
                "description": "Tool failure with a stable error_kind and optional actionable context.",
                "properties": {
                    "tool": {"type": "string"},
                    "missing": {"type": "integer", "minimum": 1},
                    "next_seq": {"type": "integer", "minimum": 1},
                    "error_kind": {
                        "type": "string",
                        "enum": [
                            "Internal",
                            "Usage",
                            "NotFound",
                            "AlreadyExists",
                            "Busy",
                            "Permission",
                            "Corrupt",
                            "Io",
                            "RetentionGap"
                        ]
                    }
                },
                "required": ["tool", "error_kind"]
            }
        ]
    })
}

fn message_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "seq": {"type": "integer", "minimum": 0},
            "time": {"type": "string"},
            "meta": {
                "type": "object",
                "properties": {
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"}
                    }
                },
                "required": ["tags"],
                "additionalProperties": false
            },
            "data": {}
        },
        "required": ["seq", "time", "meta", "data"],
        "additionalProperties": false
    })
}

fn pool_info_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {"type": "string"},
            "path": {"type": "string"},
            "file_size": {"type": "integer", "minimum": 0},
            "bounds": {
                "description": "Current retained sequence range; empty pools have no bounds.",
                "type": "object",
                "properties": {
                    "oldest": {"type": "integer", "minimum": 0},
                    "newest": {"type": "integer", "minimum": 0}
                },
                "additionalProperties": false
            },
            "metrics": {"type": "object"}
        },
        "required": ["name", "path", "file_size", "bounds"],
        "additionalProperties": true
    })
}

fn property_output_schema(name: &str, value_schema: Value) -> Value {
    let mut properties = Map::new();
    properties.insert(name.to_string(), value_schema);
    tool_output_schema(json!({
        "type": "object",
        "properties": properties,
        "required": [name],
        "additionalProperties": false
    }))
}

fn read_tool(live: bool) -> McpTool {
    let mut properties = json!({
        "pool": {"type": "string", "description": "Pool name"},
        "since": {
            "oneOf": [{"type": "integer", "minimum": 1}, {"type": "string"}],
            "description": "Inclusive sequence, oldest, RFC 3339 instant, or relative duration"
        },
        "last": {"type": "integer", "minimum": 0, "description": "Start this many records back before filtering"},
        "count": {"type": "integer", "minimum": 1, "maximum": MAX_READ_COUNT, "default": DEFAULT_READ_COUNT},
        "tags": {"type": "array", "items": {"type": "string"}, "description": "Return only messages containing all specified tags"}
    });
    if live {
        properties["timeout_ms"] = json!({
            "type": "integer", "minimum": 0, "maximum": MAX_WAIT_TIMEOUT_MS, "default": DEFAULT_WAIT_TIMEOUT_MS
        });
    }
    let (name, description) = if live {
        (
            "plasmite_follow",
            "Wait for new messages by default. Set since or last to start in retained history. Resume by passing next_seq as since.",
        )
    } else {
        (
            "plasmite_fetch",
            "Read retained messages in sequence order through the opening bound. Resume by passing next_seq as since.",
        )
    };
    McpTool {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: json!({
            "type": "object",
            "properties": properties,
            "required": ["pool"],
            "additionalProperties": false
        }),
        output_schema: tool_output_schema(json!({
            "type": "object",
            "properties": {
                "messages": {"type": "array", "items": message_output_schema()},
                "next_seq": {"type": ["integer", "null"], "minimum": 1},
                "ended": {"enum": ["complete", "limit"]}
            },
            "required": ["messages", "next_seq", "ended"],
            "additionalProperties": false
        })),
        annotations: ToolAnnotations::read_only(),
        access: McpToolAccess::Read,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCallRequest {
    pub name: String,
    pub arguments: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub content: Vec<Value>,
    #[serde(rename = "isError", default, skip_serializing_if = "is_false")]
    pub is_error: bool,
    #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
}

impl ToolCallResult {
    pub fn success(content: Vec<Value>) -> Self {
        Self {
            content,
            is_error: false,
            structured_content: None,
        }
    }

    pub fn success_with_structured(message: impl Into<String>, structured_content: Value) -> Self {
        Self {
            content: vec![json!({
                "type": "text",
                "text": message.into(),
            })],
            is_error: false,
            structured_content: Some(structured_content),
        }
    }

    pub fn execution_error_text(message: impl Into<String>) -> Self {
        Self::execution_error_with_structured(message, None)
    }

    pub fn execution_error_with_structured(
        message: impl Into<String>,
        structured_content: Option<Value>,
    ) -> Self {
        Self {
            content: vec![json!({
                "type": "text",
                "text": message.into(),
            })],
            is_error: true,
            structured_content,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpResource {
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceReadRequest {
    pub uri: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceReadResult {
    pub contents: Vec<ResourceContent>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceContent {
    pub uri: String,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

pub const EVENTS_EXTENSION: &str = "io.modelcontextprotocol/events";

pub trait McpHandler {
    fn event_catalog(&self) -> Option<&'static Value> {
        None
    }

    fn list_tools(&mut self) -> Result<Vec<McpTool>, JsonRpcError>;
    fn call_tool(&mut self, request: ToolCallRequest) -> Result<ToolCallResult, JsonRpcError>;
    fn list_resources(&mut self) -> Result<Vec<McpResource>, JsonRpcError>;
    fn read_resource(
        &mut self,
        request: ResourceReadRequest,
    ) -> Result<ResourceReadResult, JsonRpcError>;
}

pub struct McpDispatcher<H> {
    metadata: ServerMetadata,
    handler: H,
}

impl<H: McpHandler> McpDispatcher<H> {
    pub fn new(handler: H) -> Self {
        Self {
            metadata: ServerMetadata::default(),
            handler,
        }
    }

    pub fn with_metadata(handler: H, metadata: ServerMetadata) -> Self {
        Self { metadata, handler }
    }

    pub fn metadata(&self) -> &ServerMetadata {
        &self.metadata
    }

    pub fn handler_mut(&mut self) -> &mut H {
        &mut self.handler
    }

    pub fn dispatch_value(&mut self, value: Value) -> DispatchOutcome {
        self.dispatch_value_with_legacy(value, false)
    }

    pub fn dispatch_value_with_legacy(
        &mut self,
        value: Value,
        allow_legacy: bool,
    ) -> DispatchOutcome {
        match parse_jsonrpc_request(value) {
            Ok(request) => self.dispatch(request, allow_legacy),
            Err(response) => DispatchOutcome::Response(*response),
        }
    }

    pub fn dispatch_request(&mut self, request: JsonRpcRequest) -> DispatchOutcome {
        self.dispatch(request, false)
    }

    fn dispatch(&mut self, request: JsonRpcRequest, allow_legacy: bool) -> DispatchOutcome {
        let Some(id) = request.id.clone() else {
            // Notifications never invoke request methods (which may write a pool).
            return DispatchOutcome::NoResponse;
        };
        let result = if id == JsonRpcId::Null {
            Err(JsonRpcError::invalid_request(
                "MCP requests need a non-null id",
            ))
        } else if allow_legacy && !uses_modern_mcp(&request.method, request.params.as_ref()) {
            self.route_legacy(request)
        } else {
            self.route_modern(request)
        };
        DispatchOutcome::Response(match result {
            Ok(result) => JsonRpcResponse::success(id, result),
            Err(error) => JsonRpcResponse::error(id, error),
        })
    }

    fn route_legacy(&mut self, request: JsonRpcRequest) -> Result<Value, JsonRpcError> {
        let empty = Map::new();
        let params = match request.params.as_ref() {
            Some(Value::Object(params)) => params,
            None | Some(Value::Null) => &empty,
            _ => {
                return Err(JsonRpcError::invalid_params(
                    "params must be an object when provided",
                ));
            }
        };
        match request.method.as_str() {
            "initialize" => Ok(json!({
                "protocolVersion": LEGACY_MCP_PROTOCOL_VERSION,
                "capabilities": {"tools": {}, "resources": {}},
                "serverInfo": {"name": self.metadata.name, "version": self.metadata.version},
                "instructions": MCP_INSTRUCTIONS,
            })),
            "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
            _ => self
                .route_method(&request.method, params)
                .map_err(|mut error| {
                    if request.method == "resources/read"
                        && error.code == INVALID_PARAMS_CODE
                        && error
                            .data
                            .as_ref()
                            .and_then(|data| data.get("error_kind"))
                            .and_then(Value::as_str)
                            == Some("NotFound")
                    {
                        error.code = -32002;
                    }
                    error
                }),
        }
    }

    fn route_modern(&mut self, request: JsonRpcRequest) -> Result<Value, JsonRpcError> {
        let params = modern_params(request.params.as_ref())?;
        let cacheable = matches!(
            request.method.as_str(),
            "server/discover"
                | "tools/list"
                | "resources/list"
                | "resources/read"
                | "resources/templates/list"
        );
        let mut result = match request.method.as_str() {
            "server/discover" => {
                let mut capabilities = json!({"tools": {}, "resources": {}});
                if self.handler.event_catalog().is_some() {
                    capabilities["extensions"] = json!({(EVENTS_EXTENSION): {}});
                }
                json!({
                    "supportedVersions": SUPPORTED_PROTOCOL_VERSIONS,
                    "capabilities": capabilities,
                    "instructions": MCP_INSTRUCTIONS,
                })
            }
            "events/list" => match self.handler.event_catalog() {
                Some(events) => json!({"events": events}),
                None => {
                    return Err(JsonRpcError::method_not_found(
                        "method not found: events/list",
                    ));
                }
            },
            "events/poll" | "events/subscribe" | "events/unsubscribe"
                if self.handler.event_catalog().is_some() =>
            {
                let mode = if request.method == "events/poll" {
                    "poll"
                } else {
                    "webhook"
                };
                return Err(JsonRpcError {
                    code: -32026,
                    message: "Unsupported event delivery mode".into(),
                    data: Some(json!({"feature": "deliveryMode", "value": mode})),
                });
            }
            "resources/templates/list" => json!({"resourceTemplates": []}),
            _ => self.route_method(&request.method, params)?,
        };
        result["resultType"] = json!("complete");
        if cacheable {
            // Pool contents and permissions can change between requests.
            result["ttlMs"] = json!(0);
            result["cacheScope"] = json!("private");
        }
        result["_meta"][SERVER_INFO_KEY] = json!({
            "name": self.metadata.name,
            "version": self.metadata.version,
        });
        Ok(result)
    }

    fn route_method(
        &mut self,
        method: &str,
        params: &Map<String, Value>,
    ) -> Result<Value, JsonRpcError> {
        match method {
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": self.handler.list_tools()?})),
            "tools/call" => {
                let request = parse_tool_call_params(params)?;
                to_value(self.handler.call_tool(request)?)
            }
            "resources/list" => Ok(json!({"resources": self.handler.list_resources()?})),
            "resources/read" => {
                let request = parse_resource_read_params(params)?;
                to_value(self.handler.read_resource(request)?)
            }
            _ => Err(JsonRpcError::method_not_found(format!(
                "method not found: {method}"
            ))),
        }
    }
}

pub fn parse_modern_request(value: Value) -> Result<(JsonRpcId, Map<String, Value>), JsonRpcError> {
    let request = parse_jsonrpc_request(value)
        .map_err(|response| response.error.expect("failed request has an error"))?;
    let id = request
        .id
        .filter(|id| *id != JsonRpcId::Null)
        .ok_or_else(|| JsonRpcError::invalid_request("MCP requests need a non-null id"))?;
    let params = modern_params(request.params.as_ref())?;
    Ok((id, params.clone()))
}

fn modern_params(params: Option<&Value>) -> Result<&Map<String, Value>, JsonRpcError> {
    let params = params
        .and_then(Value::as_object)
        .ok_or_else(|| JsonRpcError::invalid_params("request requires object params"))?;
    let meta = params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or_else(|| JsonRpcError::invalid_params("request requires object params._meta"))?;
    let version = meta
        .get(VERSION_KEY)
        .and_then(Value::as_str)
        .ok_or_else(|| {
            JsonRpcError::invalid_params("missing or invalid protocol version in params._meta")
        })?;
    if version != MCP_PROTOCOL_VERSION {
        return Err(JsonRpcError::unsupported_version(version));
    }
    if !meta
        .get(CLIENT_CAPABILITIES_KEY)
        .is_some_and(Value::is_object)
    {
        return Err(JsonRpcError::invalid_params(
            "missing or invalid clientCapabilities in params._meta",
        ));
    }
    if let Some(info) = meta.get("io.modelcontextprotocol/clientInfo")
        && (info.get("name").and_then(Value::as_str).is_none()
            || info.get("version").and_then(Value::as_str).is_none())
    {
        return Err(JsonRpcError::invalid_params(
            "clientInfo needs string name and version",
        ));
    }
    Ok(params)
}

#[derive(Clone, Debug)]
pub struct PlasmiteMcpHandler {
    client: LocalClient,
    cancel: Option<Arc<AtomicBool>>,
}

impl PlasmiteMcpHandler {
    pub fn new(pool_dir: impl Into<PathBuf>) -> Self {
        Self::with_client(LocalClient::new().with_pool_dir(pool_dir))
    }

    pub fn with_client(client: LocalClient) -> Self {
        Self {
            client,
            cancel: None,
        }
    }

    pub fn with_cancel(mut self, cancel: Option<Arc<AtomicBool>>) -> Self {
        self.cancel = cancel;
        self
    }

    pub fn tool_access(name: &str) -> Option<McpToolAccess> {
        TOOL_CATALOG
            .iter()
            .find(|tool| tool.name == name)
            .map(|tool| tool.access)
    }

    fn tool_pool_list(&self, args: &Map<String, Value>) -> ToolCallResult {
        if let Some(result) = reject_unknown_args("plasmite_pool_list", args, &[]) {
            return result;
        }
        let pools = match self.client.list_pools() {
            Ok(pools) => pools,
            Err(err) => return api_error_tool_result("plasmite_pool_list", err),
        };
        let mut entries = pools
            .into_iter()
            .map(|info| {
                let name = pool_name_from_path(&info.path);
                (name.clone(), pool_info_json_value(&name, &info))
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        let pools_json = entries
            .into_iter()
            .map(|(_, value)| value)
            .collect::<Vec<_>>();
        let message = if pools_json.is_empty() {
            "No pools exist yet. Create one with plasmite_pool_create, or feed with create: true."
                .to_string()
        } else {
            format!("Listed {} pool(s).", pools_json.len())
        };
        ToolCallResult::success_with_structured(message, json!({ "pools": pools_json }))
    }

    fn tool_pool_create(&self, args: &Map<String, Value>) -> ToolCallResult {
        if let Some(result) = reject_unknown_args("plasmite_pool_create", args, &["name", "size"]) {
            return result;
        }
        let name = match required_string_arg(args, "name") {
            Ok(name) => name,
            Err(result) => return invalid_argument_result("plasmite_pool_create", "name", result),
        };
        let size = match optional_u64_arg(args, "size") {
            Ok(Some(size)) => size,
            Ok(None) => DEFAULT_POOL_SIZE,
            Err(result) => return invalid_argument_result("plasmite_pool_create", "size", result),
        };

        let pool_ref = PoolRef::name(name.clone());
        let info = match self
            .client
            .create_pool(&pool_ref, PoolOptions::new(size))
            .map_err(Box::new)
            .and_then(|pool| pool.info().map_err(Box::new))
        {
            Ok(info) => info,
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                return api_error_tool_result(
                    "plasmite_pool_create",
                    err.with_hint(format!(
                        "Pool `{name}` already exists and its messages are unchanged; use it as-is."
                    )),
                );
            }
            Err(err) => return api_error_tool_result("plasmite_pool_create", *err),
        };

        ToolCallResult::success_with_structured(
            format!("Created pool `{name}`."),
            json!({ "pool": pool_info_json_value(&name, &info) }),
        )
    }

    fn tool_pool_info(&self, args: &Map<String, Value>) -> ToolCallResult {
        if let Some(result) = reject_unknown_args("plasmite_pool_info", args, &["pool"]) {
            return result;
        }
        let pool = match required_string_arg(args, "pool") {
            Ok(name) => name,
            Err(result) => return invalid_argument_result("plasmite_pool_info", "pool", result),
        };
        let pool_ref = PoolRef::name(pool.clone());
        let info = match self.client.pool_info(&pool_ref) {
            Ok(info) => info,
            Err(err) => {
                return api_error_tool_result(
                    "plasmite_pool_info",
                    with_missing_pool_hint(err, &pool, false),
                );
            }
        };
        ToolCallResult::success_with_structured(
            format!("Fetched metadata for pool `{pool}`."),
            json!({ "pool": pool_info_json_value(&pool, &info) }),
        )
    }

    fn tool_pool_delete(&self, args: &Map<String, Value>) -> ToolCallResult {
        if let Some(result) = reject_unknown_args("plasmite_pool_delete", args, &["pool"]) {
            return result;
        }
        let pool = match required_string_arg(args, "pool") {
            Ok(name) => name,
            Err(result) => return invalid_argument_result("plasmite_pool_delete", "pool", result),
        };
        let pool_ref = PoolRef::name(pool.clone());
        if let Err(err) = self.client.delete_pool(&pool_ref) {
            return api_error_tool_result(
                "plasmite_pool_delete",
                with_missing_pool_hint(err, &pool, false),
            );
        }
        ToolCallResult::success_with_structured(
            format!("Deleted pool `{pool}`."),
            json!({
                "deleted": {
                    "pool": pool
                }
            }),
        )
    }

    fn tool_feed(&self, args: &Map<String, Value>) -> ToolCallResult {
        if let Some(result) =
            reject_unknown_args("plasmite_feed", args, &["pool", "data", "tags", "create"])
        {
            return result;
        }
        let pool = match required_string_arg(args, "pool") {
            Ok(name) => name,
            Err(result) => return invalid_argument_result("plasmite_feed", "pool", result),
        };
        let data = match required_value_arg(args, "data") {
            Ok(data) => data,
            Err(result) => return invalid_argument_result("plasmite_feed", "data", result),
        };
        let tags = match optional_string_array_arg(args, "tags") {
            Ok(Some(tags)) => tags,
            Ok(None) => Vec::new(),
            Err(result) => return invalid_argument_result("plasmite_feed", "tags", result),
        };
        let create = match optional_bool_arg(args, "create") {
            Ok(Some(create)) => create,
            Ok(None) => false,
            Err(result) => return invalid_argument_result("plasmite_feed", "create", result),
        };

        let pool_ref = PoolRef::name(pool.clone());
        let mut opened = match self.client.open_pool(&pool_ref, create) {
            Ok(pool) => pool,
            Err(err) => {
                return api_error_tool_result(
                    "plasmite_feed",
                    with_missing_pool_hint(err, &pool, true),
                );
            }
        };

        let message = match opened.feed(&data, Feed::new().tags(tags)) {
            Ok(message) => message,
            Err(err) => return api_error_tool_result("plasmite_feed", err),
        };

        ToolCallResult::success_with_structured(
            format!("Appended message {} to `{pool}`.", message.seq),
            Value::from_iter([("message", message_json_value(message))]),
        )
    }

    fn tool_read(&self, args: &Map<String, Value>, live: bool) -> ToolCallResult {
        let (tool, fields): (&str, &[&str]) = if live {
            (
                "plasmite_follow",
                &["pool", "since", "last", "tags", "count", "timeout_ms"],
            )
        } else {
            (
                "plasmite_fetch",
                &["pool", "since", "last", "tags", "count"],
            )
        };
        if let Some(result) = reject_unknown_args(tool, args, fields) {
            return result;
        }
        let pool = match required_string_arg(args, "pool") {
            Ok(name) => name,
            Err(detail) => return invalid_argument_result(tool, "pool", detail),
        };
        let count = match optional_usize_arg(args, "count") {
            Ok(None) => DEFAULT_READ_COUNT,
            Ok(Some(count)) if (1..=MAX_READ_COUNT).contains(&count) => count,
            Ok(Some(_)) => {
                return invalid_argument_result(
                    tool,
                    "count",
                    format!("must be between 1 and {MAX_READ_COUNT}"),
                );
            }
            Err(detail) => return invalid_argument_result(tool, "count", detail),
        };
        let wait_ms = match optional_u64_arg(args, "timeout_ms") {
            Ok(None) if live => DEFAULT_WAIT_TIMEOUT_MS,
            Ok(None) => 0,
            Ok(Some(value)) if value <= MAX_WAIT_TIMEOUT_MS => value,
            Ok(Some(_)) => {
                return invalid_argument_result(
                    tool,
                    "timeout_ms",
                    format!("must be at most {MAX_WAIT_TIMEOUT_MS}"),
                );
            }
            Err(detail) => return invalid_argument_result(tool, "timeout_ms", detail),
        };
        let mut options = match read_selection(args) {
            Ok(options) => options,
            Err((field, detail)) => return invalid_argument_result(tool, field, detail),
        };
        if let Some(cancel) = &self.cancel {
            options = options.cancel(cancel.clone());
        }
        let pool_ref = PoolRef::name(pool.clone());
        let opened = match self.client.open_pool(&pool_ref, false) {
            Ok(pool) => pool,
            Err(err) => {
                return api_error_tool_result(tool, with_missing_pool_hint(err, &pool, false));
            }
        };
        match read_batch(
            &opened,
            options,
            live,
            count,
            Duration::from_millis(wait_ms),
        ) {
            Ok(payload) => ToolCallResult::success_with_structured(
                format!("Read {} message(s) from `{pool}`.", payload.messages.len()),
                payload.into_json(),
            ),
            Err(error) => api_error_tool_result(tool, error),
        }
    }
}

impl Default for PlasmiteMcpHandler {
    fn default() -> Self {
        Self::with_client(LocalClient::new())
    }
}

impl PlasmiteMcpHandler {
    fn build_tool_catalog() -> Vec<McpTool> {
        vec![
            McpTool {
                name: "plasmite_pool_list".to_string(),
                description: "List the available named message pools. Start here when you do not already know which pool to use.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
                output_schema: property_output_schema(
                    "pools",
                    json!({
                        "type": "array",
                        "items": pool_info_output_schema()
                    }),
                ),
                annotations: ToolAnnotations::read_only(),
                access: McpToolAccess::Read,
            },
            McpTool {
                name: "plasmite_pool_create".to_string(),
                description: "Create a fixed-size message pool. Pools are append-only ring buffers and overwrite their oldest messages when full. If the pool already exists, this returns AlreadyExists and leaves its messages unchanged; use the existing pool as-is.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "name": {"type":"string","description":"Pool name"},
                        "size": {"type":"integer","description":"Fixed capacity in bytes (default: 1048576)"}
                    },
                    "required": ["name"],
                    "additionalProperties": false
                }),
                output_schema: property_output_schema("pool", pool_info_output_schema()),
                annotations: ToolAnnotations::additive_idempotent(),
                access: McpToolAccess::Write,
            },
            McpTool {
                name: "plasmite_pool_info".to_string(),
                description: "Inspect a pool's fixed capacity, retained message bounds, and metrics.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pool": {"type":"string","description":"Pool name"}
                    },
                    "required": ["pool"],
                    "additionalProperties": false
                }),
                output_schema: property_output_schema("pool", pool_info_output_schema()),
                annotations: ToolAnnotations::read_only(),
                access: McpToolAccess::Read,
            },
            McpTool {
                name: "plasmite_pool_delete".to_string(),
                description: "Permanently delete a pool and all messages it currently retains.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pool": {"type":"string","description":"Pool name"}
                    },
                    "required": ["pool"],
                    "additionalProperties": false
                }),
                output_schema: property_output_schema(
                    "deleted",
                    json!({
                        "type": "object",
                        "properties": {
                            "pool": {"type": "string"}
                        },
                        "required": ["pool"],
                        "additionalProperties": false
                    }),
                ),
                annotations: ToolAnnotations::destructive(),
                access: McpToolAccess::Write,
            },
            McpTool {
                name: "plasmite_feed".to_string(),
                description: "Append a JSON message to a pool; this never updates or replaces an existing message. The pool must exist unless `create` is true. Retrying after an ambiguous failure can append a duplicate.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pool": {"type":"string","description":"Destination pool name"},
                        "data": {"type":"object","description":"Application data"},
                        "tags": {"type":"array","items":{"type":"string"},"description":"Optional labels readers can match exactly"},
                        "create": {"type":"boolean","description":"Create a missing pool with the default fixed capacity (default: false)"}
                    },
                    "required": ["pool", "data"],
                    "additionalProperties": false
                }),
                output_schema: property_output_schema("message", message_output_schema()),
                annotations: ToolAnnotations::additive(),
                access: McpToolAccess::Write,
            },
            read_tool(false),
            read_tool(true),
        ]
    }
}

static TOOL_CATALOG: LazyLock<Vec<McpTool>> = LazyLock::new(PlasmiteMcpHandler::build_tool_catalog);

impl McpHandler for PlasmiteMcpHandler {
    fn list_tools(&mut self) -> Result<Vec<McpTool>, JsonRpcError> {
        Ok(TOOL_CATALOG.clone())
    }

    fn call_tool(&mut self, request: ToolCallRequest) -> Result<ToolCallResult, JsonRpcError> {
        let args = request.arguments;
        let result = match request.name.as_str() {
            "plasmite_pool_list" => self.tool_pool_list(&args),
            "plasmite_pool_create" => self.tool_pool_create(&args),
            "plasmite_pool_info" => self.tool_pool_info(&args),
            "plasmite_pool_delete" => self.tool_pool_delete(&args),
            "plasmite_feed" => self.tool_feed(&args),
            "plasmite_fetch" => self.tool_read(&args, false),
            "plasmite_follow" => self.tool_read(&args, true),
            _ => {
                return Err(JsonRpcError::invalid_params(format!(
                    "unknown tool: {}",
                    request.name
                )));
            }
        };
        Ok(result)
    }

    fn list_resources(&mut self) -> Result<Vec<McpResource>, JsonRpcError> {
        let pools = self.client.list_pools().map_err(api_error_jsonrpc)?;
        let mut resources = pools
            .into_iter()
            .map(|info| {
                let name = pool_name_from_path(&info.path);
                McpResource {
                    uri: format!("plasmite:///pools/{name}"),
                    name: name.clone(),
                    description: Some(format!(
                        "Latest messages from pool {name}; reading returns up to {DEFAULT_READ_COUNT} messages ({} bytes fixed capacity; oldest messages are overwritten when full)",
                        info.file_size
                    )),
                    mime_type: Some("application/json".to_string()),
                }
            })
            .collect::<Vec<_>>();
        resources.sort_by(|left, right| left.uri.cmp(&right.uri));
        Ok(resources)
    }

    fn read_resource(
        &mut self,
        request: ResourceReadRequest,
    ) -> Result<ResourceReadResult, JsonRpcError> {
        let pool =
            pool_name_from_resource_uri(&request.uri).map_err(JsonRpcError::invalid_params)?;
        let pool_ref = PoolRef::name(pool);
        let opened = self
            .client
            .open_pool(&pool_ref, false)
            .map_err(api_error_jsonrpc)?;
        let mut options = Read::last(DEFAULT_READ_COUNT as u64);
        if let Some(cancel) = &self.cancel {
            options = options.cancel(cancel.clone());
        }
        let payload = read_batch(&opened, options, false, DEFAULT_READ_COUNT, Duration::ZERO)
            .map_err(api_error_jsonrpc)?
            .into_json();
        let text = serde_json::to_string(&payload)
            .map_err(|_| JsonRpcError::internal_error("failed to encode resource payload"))?;
        Ok(ResourceReadResult {
            contents: vec![ResourceContent {
                uri: request.uri,
                mime_type: Some("application/json".to_string()),
                text: Some(text),
                blob: None,
            }],
        })
    }
}

pub fn parse_jsonrpc_line(line: &str) -> Result<Value, JsonRpcError> {
    serde_json::from_str::<Value>(line).map_err(|_| JsonRpcError::parse_error("invalid JSON"))
}

fn parse_jsonrpc_request(value: Value) -> Result<JsonRpcRequest, Box<JsonRpcResponse>> {
    let mut object = match value {
        Value::Object(object) => object,
        _ => {
            return Err(Box::new(JsonRpcResponse::error(
                JsonRpcId::Null,
                JsonRpcError::invalid_request("request must be a JSON object"),
            )));
        }
    };

    let mut id: Option<JsonRpcId> = None;
    if let Some(raw_id) = object.remove("id") {
        let parsed_id = parse_jsonrpc_id(raw_id)
            .map_err(|error| Box::new(JsonRpcResponse::error(JsonRpcId::Null, error)))?;
        id = Some(parsed_id);
    }
    let error_id = id.clone().unwrap_or(JsonRpcId::Null);

    let jsonrpc = object
        .remove("jsonrpc")
        .and_then(|value| value.as_str().map(ToString::to_string))
        .ok_or_else(|| {
            Box::new(JsonRpcResponse::error(
                error_id.clone(),
                JsonRpcError::invalid_request("missing jsonrpc field"),
            ))
        })?;
    if jsonrpc != JSON_RPC_VERSION {
        return Err(Box::new(JsonRpcResponse::error(
            error_id,
            JsonRpcError::invalid_request("jsonrpc must be \"2.0\""),
        )));
    }

    let method = object
        .remove("method")
        .and_then(|value| value.as_str().map(ToString::to_string))
        .ok_or_else(|| {
            Box::new(JsonRpcResponse::error(
                id.clone().unwrap_or(JsonRpcId::Null),
                JsonRpcError::invalid_request("missing method field"),
            ))
        })?;

    let params = object.remove("params");
    Ok(JsonRpcRequest {
        jsonrpc,
        id,
        method,
        params,
    })
}

fn parse_jsonrpc_id(value: Value) -> Result<JsonRpcId, JsonRpcError> {
    match value {
        Value::String(value) => Ok(JsonRpcId::String(value)),
        Value::Number(value) => value
            .as_i64()
            .map(JsonRpcId::Number)
            .ok_or_else(|| JsonRpcError::invalid_request("id must be an integer number")),
        Value::Null => Ok(JsonRpcId::Null),
        _ => Err(JsonRpcError::invalid_request(
            "id must be a string, integer number, or null",
        )),
    }
}

fn parse_tool_call_params(params: &Map<String, Value>) -> Result<ToolCallRequest, JsonRpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| JsonRpcError::invalid_params("tools/call requires string param `name`"))?
        .to_string();

    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(arguments)) => arguments.clone(),
        Some(_) => {
            return Err(JsonRpcError::invalid_params(
                "tools/call `arguments` must be an object",
            ));
        }
    };

    Ok(ToolCallRequest { name, arguments })
}

fn parse_resource_read_params(
    params: &Map<String, Value>,
) -> Result<ResourceReadRequest, JsonRpcError> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| JsonRpcError::invalid_params("resources/read requires string param `uri`"))?
        .to_string();
    Ok(ResourceReadRequest { uri })
}

fn to_value<T: Serialize>(value: T) -> Result<Value, JsonRpcError> {
    serde_json::to_value(value).map_err(|_| JsonRpcError::internal_error("failed to encode result"))
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn required_string_arg(args: &Map<String, Value>, key: &str) -> Result<String, String> {
    match args.get(key) {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(format!("`{key}` must be a string")),
        None => Err(format!("missing required `{key}` argument")),
    }
}

fn required_value_arg(args: &Map<String, Value>, key: &str) -> Result<Value, String> {
    args.get(key)
        .cloned()
        .ok_or_else(|| format!("missing required `{key}` argument"))
}

fn read_selection(args: &Map<String, Value>) -> Result<Read, (&'static str, String)> {
    let last = optional_u64_arg(args, "last").map_err(|detail| ("last", detail))?;
    let options = match (args.get("since"), last) {
        (Some(_), Some(_)) => return Err(("since", "use either since or last".into())),
        (Some(Value::String(value)), None) => Read::since(
            value
                .parse::<Since>()
                .map_err(|err| ("since", err.to_string()))?,
        ),
        (Some(value), None) => Read::since(Since::Seq(value.as_u64().ok_or_else(|| {
            (
                "since",
                "expected a positive sequence or time selection".into(),
            )
        })?)),
        (None, Some(last)) => Read::last(last),
        (None, None) => Read::new(),
    };
    let tags = optional_string_array_arg(args, "tags")
        .map_err(|detail| ("tags", detail))?
        .unwrap_or_default();
    Ok(options.tags(tags))
}

fn optional_u64_arg(args: &Map<String, Value>, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        Some(value) => match value.as_u64() {
            Some(value) => Ok(Some(value)),
            None => Err(format!("`{key}` must be a non-negative integer")),
        },
        None => Ok(None),
    }
}

fn optional_usize_arg(args: &Map<String, Value>, key: &str) -> Result<Option<usize>, String> {
    match optional_u64_arg(args, key)? {
        Some(value) => usize::try_from(value)
            .map(Some)
            .map_err(|_| format!("`{key}` is too large")),
        None => Ok(None),
    }
}

fn optional_bool_arg(args: &Map<String, Value>, key: &str) -> Result<Option<bool>, String> {
    match args.get(key) {
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(format!("`{key}` must be a boolean")),
    }
}

fn reject_unknown_args(
    tool: &str,
    args: &Map<String, Value>,
    allowed: &[&str],
) -> Option<ToolCallResult> {
    let field = args
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))?;
    Some(invalid_argument_result(tool, field, "unknown argument"))
}

fn optional_string_array_arg(
    args: &Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<String>>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    let Value::Array(values) = value else {
        return Err(format!("`{key}` must be an array of strings"));
    };
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let Value::String(value) = value else {
            return Err(format!("`{key}` must be an array of strings"));
        };
        out.push(value.clone());
    }
    Ok(Some(out))
}

fn pool_name_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(ToString::to_string)
        .unwrap_or_else(|| path.display().to_string())
}

fn pool_info_json_value(pool_ref: &str, info: &PoolInfo) -> Value {
    serde_json::to_value(crate::interface_wire::pool_info_wire!(pool_ref, info))
        .expect("pool-info wire data is serializable")
}

fn message_json_value(message: crate::api::Message) -> Value {
    MessageWire::new(
        message.seq,
        message.time_rfc3339(),
        message.meta.tags,
        message.data,
    )
    .into_json()
}

struct ReadBatch {
    messages: Vec<Value>,
    next_seq: Option<u64>,
    ended: &'static str,
}

impl ReadBatch {
    fn into_json(self) -> Value {
        json!({"messages": self.messages, "next_seq": self.next_seq, "ended": self.ended})
    }
}

fn read_batch(
    pool: &Pool,
    options: Read,
    live: bool,
    count: usize,
    wait: Duration,
) -> Result<ReadBatch, Error> {
    let mut reader = if live {
        pool.follow_lite3(options)?
    } else {
        pool.fetch_lite3(options)?
    };
    let (_, upper) = reader.position();
    collect_batch(count, wait, upper, |remaining| {
        let result = reader.poll_selected(remaining, |frame, tags| {
            let message = if crate::api::frame_matches_tags(&frame, tags)? {
                Some(message_json_value(Message::from_lite3(
                    frame.seq,
                    frame.time,
                    &frame.payload,
                )?))
            } else {
                None
            };
            // Even a filtered record counts against the per-call scan limit.
            Ok(Some(message))
        });
        (result, reader.position().0)
    })
}

fn collect_batch(
    count: usize,
    wait: Duration,
    upper: Option<u64>,
    mut poll: impl FnMut(Duration) -> (Result<ReadStatus<Option<Value>>, Error>, Option<u64>),
) -> Result<ReadBatch, Error> {
    let deadline = Instant::now() + wait;
    let mut messages = Vec::with_capacity(count);
    let mut examined = 0;
    loop {
        let (result, mut next_seq) = poll(deadline.saturating_duration_since(Instant::now()));
        let ended = match result {
            Ok(ReadStatus::Message(message)) => {
                examined += 1;
                messages.extend(message);
                if upper.is_some() && (next_seq.is_none() || next_seq > upper) {
                    "complete"
                } else if messages.len() == count || examined == MAX_EXAMINED_COUNT {
                    "limit"
                } else {
                    continue;
                }
            }
            Ok(ReadStatus::Waiting) => "limit",
            Ok(ReadStatus::Complete) => "complete",
            Ok(ReadStatus::Cancelled) => {
                return Err(
                    Error::new(ErrorKind::Permission).with_message("access key was revoked")
                );
            }
            Err(error) if !messages.is_empty() => {
                // Return the prefix first. Reopening at this checkpoint reports
                // the unread record or the retention gap before any later data.
                next_seq = error.missing().or(next_seq);
                "limit"
            }
            Err(error) => return Err(error),
        };
        return Ok(ReadBatch {
            messages,
            next_seq,
            ended,
        });
    }
}

fn invalid_argument_result(tool: &str, field: &str, detail: impl Into<String>) -> ToolCallResult {
    let detail = detail.into();
    ToolCallResult::execution_error_with_structured(
        format!("Invalid `{field}` for {tool}: {detail}"),
        Some(json!({
            "tool": tool,
            "error_kind": "Usage",
            "field": field,
            "detail": detail,
        })),
    )
}

fn with_missing_pool_hint(err: Error, pool: &str, allow_create: bool) -> Error {
    if err.kind() != ErrorKind::NotFound || err.hint().is_some() {
        return err;
    }
    if allow_create {
        err.with_hint(format!(
            "Pool `{pool}` does not exist; retry plasmite_feed with create: true, or call plasmite_pool_create first."
        ))
    } else {
        err.with_hint(format!(
            "Pool `{pool}` does not exist; check the name or call plasmite_pool_list when available."
        ))
    }
}

fn api_error_tool_result(tool: &str, err: Error) -> ToolCallResult {
    let policy = error_policy(interface_error_kind(err.kind()));
    let mut text = err
        .message()
        .map(ToString::to_string)
        .unwrap_or_else(|| policy.mcp_error_kind.to_string());
    if let Some(hint) = err.hint() {
        text = format!("{text}. {hint}");
    }

    let mut structured = Map::new();
    structured.insert("tool".to_string(), json!(tool));
    structured.insert(
        "error_kind".to_string(),
        json!(error_policy(interface_error_kind(err.kind())).mcp_error_kind),
    );
    if let Some(message) = err.message() {
        structured.insert("message".to_string(), json!(message));
    }
    if let Some(hint) = err.hint() {
        structured.insert("hint".to_string(), json!(hint));
    }
    if let Some(path) = err.path() {
        structured.insert("path".to_string(), json!(path.display().to_string()));
    }
    if let Some(seq) = err.seq() {
        structured.insert("seq".to_string(), json!(seq));
    }
    if let Some(offset) = err.offset() {
        structured.insert("offset".to_string(), json!(offset));
    }

    if let Some((missing, next_seq)) = err.missing().zip(err.next_seq()) {
        structured.insert("missing".into(), json!(missing));
        structured.insert("next_seq".into(), json!(next_seq));
    }

    ToolCallResult::execution_error_with_structured(text, Some(Value::Object(structured)))
}

fn api_error_jsonrpc(err: Error) -> JsonRpcError {
    let policy = error_policy(interface_error_kind(err.kind()));
    let code = if err.kind() == ErrorKind::NotFound {
        INVALID_PARAMS_CODE
    } else {
        -32000
    };
    let mut rpc_error = JsonRpcError::new(
        code,
        err.message()
            .map(ToString::to_string)
            .unwrap_or_else(|| policy.mcp_error_kind.to_string()),
    );
    let mut data = Map::new();
    data.insert("error_kind".to_string(), json!(policy.mcp_error_kind));
    if let Some(hint) = err.hint() {
        data.insert("hint".to_string(), json!(hint));
    }
    if let Some(path) = err.path() {
        data.insert("path".to_string(), json!(path.display().to_string()));
    }
    if let Some(seq) = err.seq() {
        data.insert("seq".to_string(), json!(seq));
    }
    if let Some(offset) = err.offset() {
        data.insert("offset".to_string(), json!(offset));
    }
    if let Some((missing, next_seq)) = err.missing().zip(err.next_seq()) {
        data.insert("missing".into(), json!(missing));
        data.insert("next_seq".into(), json!(next_seq));
    }
    if !data.is_empty() {
        rpc_error.data = Some(Value::Object(data));
    }
    rpc_error
}

fn interface_error_kind(kind: ErrorKind) -> ErrorKindWire {
    match kind {
        ErrorKind::Internal => ErrorKindWire::Internal,
        ErrorKind::Usage => ErrorKindWire::Usage,
        ErrorKind::NotFound => ErrorKindWire::NotFound,
        ErrorKind::AlreadyExists => ErrorKindWire::AlreadyExists,
        ErrorKind::Busy => ErrorKindWire::Busy,
        ErrorKind::Permission => ErrorKindWire::Permission,
        ErrorKind::Corrupt => ErrorKindWire::Corrupt,
        ErrorKind::Io => ErrorKindWire::Io,
        ErrorKind::RetentionGap => ErrorKindWire::RetentionGap,
    }
}

fn pool_name_from_resource_uri(uri: &str) -> Result<String, String> {
    let parsed =
        url::Url::parse(uri).map_err(|_| "resource uri must be a valid URI".to_string())?;
    if parsed.scheme() != "plasmite" {
        return Err("resource uri must use plasmite scheme".to_string());
    }
    if parsed.host_str().is_some() {
        return Err("resource uri must be in plasmite:///pools/{name} format".to_string());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("resource uri must not include query or fragment".to_string());
    }
    let segments = parsed
        .path_segments()
        .ok_or_else(|| "resource uri must be in plasmite:///pools/{name} format".to_string())?
        .collect::<Vec<_>>();
    if segments.len() != 2 || segments[0] != "pools" || segments[1].is_empty() {
        return Err("resource uri must be in plasmite:///pools/{name} format".to_string());
    }
    Ok(segments[1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{AppendOptions, PoolApiExt};

    #[derive(Default)]
    struct StubHandler {
        list_tools_calls: usize,
        call_tool_requests: Vec<ToolCallRequest>,
        list_resources_calls: usize,
        read_resource_requests: Vec<ResourceReadRequest>,
        next_tool_result: Option<ToolCallResult>,
    }

    impl McpHandler for StubHandler {
        fn list_tools(&mut self) -> Result<Vec<McpTool>, JsonRpcError> {
            self.list_tools_calls += 1;
            Ok(vec![McpTool {
                name: "plasmite_pool_list".to_string(),
                description: "List pools".to_string(),
                input_schema: json!({"type":"object","properties":{}}),
                output_schema: property_output_schema("pools", json!({"type": "array"})),
                annotations: ToolAnnotations::read_only(),
                access: McpToolAccess::Read,
            }])
        }

        fn call_tool(&mut self, request: ToolCallRequest) -> Result<ToolCallResult, JsonRpcError> {
            self.call_tool_requests.push(request);
            Ok(self.next_tool_result.clone().unwrap_or_else(|| {
                ToolCallResult::success(vec![json!({"type":"text","text":"ok"})])
            }))
        }

        fn list_resources(&mut self) -> Result<Vec<McpResource>, JsonRpcError> {
            self.list_resources_calls += 1;
            Ok(vec![McpResource {
                uri: "plasmite:///pools/events".to_string(),
                name: "events".to_string(),
                description: Some("Events pool".to_string()),
                mime_type: Some("application/json".to_string()),
            }])
        }

        fn read_resource(
            &mut self,
            request: ResourceReadRequest,
        ) -> Result<ResourceReadResult, JsonRpcError> {
            self.read_resource_requests.push(request);
            Ok(ResourceReadResult {
                contents: vec![ResourceContent {
                    uri: "plasmite:///pools/events".to_string(),
                    mime_type: Some("application/json".to_string()),
                    text: Some("{\"messages\":[]}".to_string()),
                    blob: None,
                }],
            })
        }
    }

    fn request(id: JsonRpcId, method: &str, params: Option<Value>) -> Value {
        let mut object = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
        });
        let mut params = params.unwrap_or_else(|| json!({}));
        params["_meta"] = json!({
            VERSION_KEY: MCP_PROTOCOL_VERSION,
            CLIENT_CAPABILITIES_KEY: {}
        });
        object["params"] = params;
        object
    }

    fn expect_response(outcome: DispatchOutcome) -> JsonRpcResponse {
        match outcome {
            DispatchOutcome::Response(response) => response,
            DispatchOutcome::NoResponse => panic!("expected response"),
        }
    }

    #[test]
    fn discover_reports_protocol_capabilities_and_server_info() {
        let handler = StubHandler::default();
        let mut dispatcher = McpDispatcher::new(handler);
        let response = expect_response(dispatcher.dispatch_value(request(
            JsonRpcId::Number(1),
            "server/discover",
            None,
        )));
        let result = response.result.expect("result");
        assert_eq!(response.error, None);
        assert_eq!(
            result["supportedVersions"],
            json!([MCP_PROTOCOL_VERSION, LEGACY_MCP_PROTOCOL_VERSION])
        );
        assert_eq!(
            result["capabilities"],
            json!({"tools": {}, "resources": {}})
        );
        assert_eq!(result["_meta"][SERVER_INFO_KEY]["name"], "plasmite");
        assert!(result["instructions"].as_str().is_some_and(|instructions| {
            instructions.contains("append-only ring buffers")
                && instructions.contains("most users can ignore sequence numbers")
                && instructions.contains("target pool is unknown")
                && instructions.contains("visible tool list is the server's effective access mode")
                && instructions.contains("Use plasmite_fetch for retained messages")
        }));
    }

    #[test]
    fn initialized_notification_returns_no_response() {
        let handler = StubHandler::default();
        let mut dispatcher = McpDispatcher::new(handler);
        let outcome = dispatcher.dispatch_value(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        }));
        assert_eq!(outcome, DispatchOutcome::NoResponse);
    }

    #[test]
    fn ping_routes_and_returns_empty_object() {
        let handler = StubHandler::default();
        let mut dispatcher = McpDispatcher::new(handler);
        let response = expect_response(dispatcher.dispatch_value(request(
            JsonRpcId::Number(2),
            "ping",
            Some(json!({})),
        )));
        assert_eq!(response.error, None);
        assert_eq!(response.result.unwrap()["resultType"], "complete");
    }

    #[test]
    fn tools_list_and_call_route_to_handler() {
        let handler = StubHandler::default();
        let mut dispatcher = McpDispatcher::new(handler);

        let list_response = expect_response(dispatcher.dispatch_value(request(
            JsonRpcId::Number(3),
            "tools/list",
            Some(json!({})),
        )));
        assert_eq!(list_response.error, None);
        let list_result = list_response.result.expect("tools list result");
        let tools = list_result["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], json!("plasmite_pool_list"));
        assert_eq!(
            tools[0]["inputSchema"],
            json!({"type":"object","properties":{}})
        );
        assert_eq!(tools[0]["outputSchema"]["type"], json!("object"));
        assert_eq!(
            tools[0]["annotations"],
            json!({
                "readOnlyHint":true,
                "destructiveHint":false,
                "idempotentHint":true,
                "openWorldHint":false
            })
        );

        let call_response = expect_response(dispatcher.dispatch_value(request(
            JsonRpcId::Number(4),
            "tools/call",
            Some(json!({
                "name": "plasmite_pool_list",
                "arguments": {"pool":"events"}
            })),
        )));
        assert_eq!(call_response.error, None);
        let call_result = call_response.result.expect("tools call result");
        assert_eq!(call_result["content"][0]["text"], json!("ok"));

        assert_eq!(dispatcher.handler_mut().list_tools_calls, 1);
        assert_eq!(dispatcher.handler_mut().call_tool_requests.len(), 1);
        assert_eq!(
            dispatcher.handler_mut().call_tool_requests[0].arguments["pool"],
            json!("events")
        );
    }

    #[test]
    fn resources_list_and_read_route_to_handler() {
        let handler = StubHandler::default();
        let mut dispatcher = McpDispatcher::new(handler);

        let list_response = expect_response(dispatcher.dispatch_value(request(
            JsonRpcId::Number(5),
            "resources/list",
            Some(json!({})),
        )));
        assert_eq!(list_response.error, None);
        assert_eq!(
            list_response.result.expect("resources list result")["resources"][0]["uri"],
            json!("plasmite:///pools/events")
        );

        let read_response = expect_response(dispatcher.dispatch_value(request(
            JsonRpcId::Number(6),
            "resources/read",
            Some(json!({
                "uri": "plasmite:///pools/events"
            })),
        )));
        assert_eq!(read_response.error, None);
        assert_eq!(
            read_response.result.expect("resources read result")["contents"][0]["uri"],
            json!("plasmite:///pools/events")
        );

        assert_eq!(dispatcher.handler_mut().list_resources_calls, 1);
        assert_eq!(dispatcher.handler_mut().read_resource_requests.len(), 1);
        assert_eq!(
            dispatcher.handler_mut().read_resource_requests[0].uri,
            "plasmite:///pools/events"
        );
    }

    #[test]
    fn unknown_method_returns_protocol_error() {
        let handler = StubHandler::default();
        let mut dispatcher = McpDispatcher::new(handler);
        let response = expect_response(dispatcher.dispatch_value(request(
            JsonRpcId::String("abc".to_string()),
            "tools/unknown",
            Some(json!({})),
        )));
        let error = response.error.expect("error");
        assert_eq!(error.code, METHOD_NOT_FOUND_CODE);
        assert!(error.message.contains("method not found"));
        assert_eq!(response.result, None);
    }

    #[test]
    fn malformed_tools_call_params_return_protocol_error() {
        let handler = StubHandler::default();
        let mut dispatcher = McpDispatcher::new(handler);
        let response = expect_response(dispatcher.dispatch_value(request(
            JsonRpcId::Number(7),
            "tools/call",
            Some(json!({
                "name": "plasmite_feed",
                "arguments": "not-an-object"
            })),
        )));
        let error = response.error.expect("error");
        assert_eq!(error.code, INVALID_PARAMS_CODE);
        assert_eq!(response.result, None);
    }

    #[test]
    fn tool_execution_error_returns_success_with_is_error() {
        let handler = StubHandler {
            next_tool_result: Some(ToolCallResult::execution_error_text("pool not found")),
            ..StubHandler::default()
        };
        let mut dispatcher = McpDispatcher::new(handler);
        let response = expect_response(dispatcher.dispatch_value(request(
            JsonRpcId::Number(8),
            "tools/call",
            Some(json!({
                "name": "plasmite_fetch",
                "arguments": {"pool":"missing"}
            })),
        )));
        assert_eq!(response.error, None);
        let result = response.result.expect("result");
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["content"][0]["text"], json!("pool not found"));
    }

    #[test]
    fn mcp_error_presenter_preserves_every_stable_error_kind() {
        let cases = [
            (ErrorKind::Internal, "Internal"),
            (ErrorKind::Usage, "Usage"),
            (ErrorKind::NotFound, "NotFound"),
            (ErrorKind::AlreadyExists, "AlreadyExists"),
            (ErrorKind::Busy, "Busy"),
            (ErrorKind::Permission, "Permission"),
            (ErrorKind::Corrupt, "Corrupt"),
            (ErrorKind::Io, "Io"),
            (ErrorKind::RetentionGap, "RetentionGap"),
        ];

        for (kind, name) in cases {
            let result = api_error_tool_result("plasmite_test", Error::new(kind));
            assert!(result.is_error);
            assert_eq!(
                result.structured_content.expect("structured error")["error_kind"],
                json!(name)
            );
        }
    }

    fn map_args(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object args")
    }

    fn read_messages_from_result(result: &ToolCallResult) -> Vec<Value> {
        result
            .structured_content
            .as_ref()
            .expect("structured content")
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages")
            .clone()
    }

    fn seed_pool_with_messages(
        handler: &mut PlasmiteMcpHandler,
        pool: &str,
        count: u64,
        base_ns: u64,
    ) {
        let create_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_pool_create".to_string(),
                arguments: map_args(json!({"name": pool})),
            })
            .expect("call create");
        assert!(!create_result.is_error);

        let pool_ref = PoolRef::name(pool.to_string());
        let mut opened = handler
            .client
            .open_pool(&pool_ref, false)
            .expect("open pool");
        for idx in 0..count {
            let timestamp_ns = base_ns.saturating_add(idx.saturating_mul(1_000_000_000));
            opened
                .append_json(
                    &json!({"value": idx + 1}),
                    &[],
                    AppendOptions::new(timestamp_ns, Durability::Fast),
                )
                .expect("append");
        }
    }

    #[test]
    fn plasmite_tools_execute_against_local_api() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());

        let create_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_pool_create".to_string(),
                arguments: map_args(json!({"name":"demo"})),
            })
            .expect("create");
        assert!(!create_result.is_error);

        let feed_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_feed".to_string(),
                arguments: map_args(json!({
                    "pool": "demo",
                    "data": {"from":"alice","msg":"hello"},
                    "tags": ["chat"]
                })),
            })
            .expect("feed");
        assert!(!feed_result.is_error);
        let seq = feed_result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("message"))
            .and_then(|value| value.get("seq"))
            .and_then(Value::as_u64)
            .expect("seq");

        let fetch_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_fetch".to_string(),
                arguments: map_args(json!({
                    "pool": "demo",
                    "since": seq, "count": 1
                })),
            })
            .expect("fetch");
        assert!(!fetch_result.is_error);
        assert_eq!(
            fetch_result
                .structured_content
                .as_ref()
                .expect("structured")
                .pointer("/messages/0/data")
                .and_then(|value| value.get("msg"))
                .and_then(Value::as_str),
            Some("hello")
        );

        let list_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_pool_list".to_string(),
                arguments: Map::new(),
            })
            .expect("list");
        assert!(!list_result.is_error);
        let listed = list_result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("pools"))
            .and_then(Value::as_array)
            .expect("pools");
        assert!(
            listed
                .iter()
                .any(|pool| pool.get("name").and_then(Value::as_str) == Some("demo"))
        );

        let delete_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_pool_delete".to_string(),
                arguments: map_args(json!({"pool":"demo"})),
            })
            .expect("delete");
        assert!(!delete_result.is_error);
    }

    #[test]
    fn plasmite_pool_create_preserves_existing_pool() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        seed_pool_with_messages(&mut handler, "events", 1, 1_700_000_000_000_000_000);

        let create_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_pool_create".to_string(),
                arguments: map_args(json!({"name":"events", "size": 2 * 1024 * 1024})),
            })
            .expect("create");
        assert!(create_result.is_error);
        assert_eq!(
            create_result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("error_kind"))
                .and_then(Value::as_str),
            Some("AlreadyExists")
        );
        let hint = create_result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("hint"))
            .and_then(Value::as_str)
            .expect("hint");
        assert!(hint.contains("use it as-is"));
        assert!(!hint.contains("delete"));

        let read_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_fetch".to_string(),
                arguments: map_args(json!({"pool":"events"})),
            })
            .expect("read");
        assert_eq!(read_messages_from_result(&read_result).len(), 1);
    }

    #[test]
    fn plasmite_pool_list_and_resources_are_empty_before_first_create() {
        let tmp = tempfile::tempdir().expect("tmp");
        let pool_dir = tmp.path().join("missing");
        let mut handler = PlasmiteMcpHandler::new(&pool_dir);

        let list_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_pool_list".to_string(),
                arguments: Map::new(),
            })
            .expect("list");
        assert!(!list_result.is_error);
        assert_eq!(list_result.structured_content, Some(json!({"pools": []})));
        assert!(
            list_result.content[0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("No pools exist yet"))
        );
        assert!(handler.list_resources().expect("resources").is_empty());
        assert!(!pool_dir.exists());
    }

    #[test]
    fn plasmite_missing_pool_errors_explain_recovery() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());

        let feed_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_feed".to_string(),
                arguments: map_args(json!({"pool": "missing", "data": {}})),
            })
            .expect("feed");
        assert!(feed_result.is_error);
        assert!(
            feed_result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("hint"))
                .and_then(Value::as_str)
                .is_some_and(|hint| hint.contains("create: true"))
        );

        let read_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_fetch".to_string(),
                arguments: map_args(json!({"pool": "missing"})),
            })
            .expect("read");
        assert!(read_result.is_error);
        assert!(
            read_result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("hint"))
                .and_then(Value::as_str)
                .is_some_and(|hint| hint.contains("plasmite_pool_list"))
        );
    }

    #[test]
    fn plasmite_invalid_pool_size_does_not_poison_listing() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());

        let create_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_pool_create".to_string(),
                arguments: map_args(json!({"name": "too-small", "size": 1024})),
            })
            .expect("create");
        assert!(create_result.is_error);
        assert_eq!(
            create_result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("error_kind"))
                .and_then(Value::as_str),
            Some("Usage")
        );

        let list_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_pool_list".to_string(),
                arguments: Map::new(),
            })
            .expect("list");
        assert!(!list_result.is_error);
        assert_eq!(list_result.structured_content, Some(json!({"pools": []})));
        assert!(!tmp.path().join("too-small.plasmite").exists());
    }

    #[test]
    fn plasmite_tools_reject_unknown_arguments() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());

        for (field, detail) in [("where", "unknown argument"), ("limit", "unknown argument")] {
            let result = handler
                .call_tool(ToolCallRequest {
                    name: "plasmite_fetch".to_string(),
                    arguments: map_args(json!({"pool": "events", (field): true})),
                })
                .expect("read");
            assert!(result.is_error);
            let structured = result.structured_content.expect("structured");
            assert_eq!(structured["error_kind"], json!("Usage"));
            assert_eq!(structured["field"], json!(field));
            assert!(
                structured["detail"]
                    .as_str()
                    .is_some_and(|value| value.contains(detail))
            );
        }
    }

    #[test]
    fn plasmite_tools_reject_non_string_tags() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        let result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_feed".to_string(),
                arguments: map_args(json!({
                    "pool": "events",
                    "data": {},
                    "tags": [false]
                })),
            })
            .expect("feed");

        assert!(result.is_error);
        let structured = result.structured_content.expect("structured");
        assert_eq!(structured["error_kind"], json!("Usage"));
        assert_eq!(structured["field"], json!("tags"));
    }

    #[test]
    fn plasmite_feed_and_fetch_preserve_tag_whitespace() {
        let tmp = tempfile::tempdir().expect("tmp");
        let handler = PlasmiteMcpHandler::new(tmp.path());
        for tag in ["sensor A ", "sensor A", " ", ""] {
            let fed = handler.tool_feed(&map_args(json!({
                "pool": "events", "data": {}, "create": true, "tags": [tag]
            })));
            assert!(!fed.is_error);
            let result = fed.structured_content.expect("feed data");
            assert_eq!(result["message"]["meta"]["tags"], json!([tag]));
        }
        for (seq, tag) in [(1, "sensor A "), (2, "sensor A"), (3, " "), (4, "")] {
            let fetched = handler.tool_read(
                &map_args(json!({
                    "pool": "events", "tags": [tag]
                })),
                false,
            );
            assert!(!fetched.is_error);
            let result = fetched.structured_content.expect("fetch data");
            assert_eq!(result["messages"].as_array().unwrap().len(), 1);
            assert_eq!(result["messages"][0]["seq"], seq);
            assert_eq!(result["messages"][0]["meta"]["tags"], json!([tag]));
            assert_eq!(result["next_seq"], 5);
        }
    }

    #[test]
    fn plasmite_tools_advertise_accurate_behavior_annotations() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        let tools = handler.list_tools().expect("list tools");

        for name in [
            "plasmite_pool_list",
            "plasmite_pool_info",
            "plasmite_fetch",
            "plasmite_follow",
        ] {
            let annotations = &tools
                .iter()
                .find(|tool| tool.name == name)
                .unwrap_or_else(|| panic!("missing tool: {name}"))
                .annotations;
            assert_eq!(annotations, &ToolAnnotations::read_only());
        }

        let create_annotations = &tools
            .iter()
            .find(|tool| tool.name == "plasmite_pool_create")
            .expect("missing create tool")
            .annotations;
        assert_eq!(create_annotations, &ToolAnnotations::additive_idempotent());

        let feed_annotations = &tools
            .iter()
            .find(|tool| tool.name == "plasmite_feed")
            .expect("missing feed tool")
            .annotations;
        assert_eq!(feed_annotations, &ToolAnnotations::additive());

        let delete_annotations = &tools
            .iter()
            .find(|tool| tool.name == "plasmite_pool_delete")
            .expect("missing delete tool")
            .annotations;
        assert_eq!(delete_annotations, &ToolAnnotations::destructive());
    }

    #[test]
    fn plasmite_tools_advertise_structured_output_schemas() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        let tools = handler.list_tools().expect("list tools");

        assert_eq!(tools.len(), 7);
        for tool in &tools {
            assert_eq!(tool.output_schema["type"], json!("object"), "{}", tool.name);
            assert_eq!(
                tool.output_schema["oneOf"].as_array().map(Vec::len),
                Some(2),
                "{}",
                tool.name
            );
            assert_eq!(
                tool.output_schema["oneOf"][1]["properties"]["error_kind"]["enum"],
                json!([
                    "Internal",
                    "Usage",
                    "NotFound",
                    "AlreadyExists",
                    "Busy",
                    "Permission",
                    "Corrupt",
                    "Io",
                    "RetentionGap"
                ]),
                "{}",
                tool.name
            );
        }

        let read = tools
            .iter()
            .find(|tool| tool.name == "plasmite_fetch")
            .expect("read tool");
        assert!(read.input_schema["properties"].get("where").is_none());
        assert_eq!(read.input_schema["additionalProperties"], json!(false));
        assert_eq!(
            read.input_schema["properties"]["count"]["minimum"],
            json!(1)
        );
        assert!(
            read.input_schema["properties"]["tags"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("all specified tags"))
        );
        assert_eq!(
            read.output_schema["oneOf"][0]["properties"]["next_seq"]["minimum"],
            json!(1)
        );
        let wait = tools
            .iter()
            .find(|tool| tool.name == "plasmite_follow")
            .expect("wait tool");
        assert_eq!(wait.input_schema["required"], json!(["pool"]));
        assert_eq!(
            wait.output_schema["oneOf"][0]["properties"]["ended"]["enum"],
            json!(["complete", "limit"])
        );
    }

    #[test]
    fn plasmite_follow_without_cursor_starts_at_live_tail() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        seed_pool_with_messages(&mut handler, "events", 3, 1_700_000_000_000_000_000);

        let wait_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_follow".to_string(),
                arguments: map_args(json!({
                    "pool": "events",
                    "timeout_ms": 10
                })),
            })
            .expect("wait");
        assert!(!wait_result.is_error);
        let structured = wait_result.structured_content.expect("structured");
        assert_eq!(structured["messages"], json!([]));
        assert_eq!(structured["next_seq"], json!(4));
        assert!(matches!(
            structured["ended"].as_str(),
            Some("limit" | "complete")
        ));
    }

    #[test]
    fn plasmite_follow_returns_available_messages_after_cursor() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        seed_pool_with_messages(&mut handler, "events", 3, 1_700_000_000_000_000_000);

        let wait_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_follow".to_string(),
                arguments: map_args(json!({
                    "pool": "events",
                    "since": 2,
                    "timeout_ms": 50
                })),
            })
            .expect("wait");
        assert!(!wait_result.is_error);
        let structured = wait_result.structured_content.expect("structured");
        assert_eq!(structured["next_seq"], json!(4));
        assert!(matches!(
            structured["ended"].as_str(),
            Some("limit" | "complete")
        ));
        assert_eq!(
            structured["messages"].as_array().expect("messages").len(),
            2
        );
    }

    #[test]
    fn plasmite_follow_returns_bounded_timeout() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        seed_pool_with_messages(&mut handler, "events", 1, 1_700_000_000_000_000_000);

        let wait_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_follow".to_string(),
                arguments: map_args(json!({
                    "pool": "events",
                    "since": 2,
                    "timeout_ms": 10
                })),
            })
            .expect("wait");
        assert!(!wait_result.is_error);
        let structured = wait_result.structured_content.expect("structured");
        assert_eq!(structured["next_seq"], json!(2));
        assert_eq!(structured["messages"], json!([]));
    }

    #[test]
    fn plasmite_follow_timeout_advances_past_filtered_messages() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        seed_pool_with_messages(&mut handler, "events", 3, 1_700_000_000_000_000_000);

        let wait_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_follow".to_string(),
                arguments: map_args(json!({
                    "pool": "events",
                    "since": 1,
                    "tags": ["missing"],
                    "timeout_ms": 10
                })),
            })
            .expect("wait");
        let structured = wait_result.structured_content.expect("structured");
        assert_eq!(structured["messages"], json!([]));
        assert_eq!(structured["next_seq"], json!(4));
        assert!(matches!(
            structured["ended"].as_str(),
            Some("limit" | "complete")
        ));
    }

    #[test]
    fn plasmite_fetch_advances_cursor_past_filtered_messages() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        seed_pool_with_messages(&mut handler, "events", 3, 1_700_000_000_000_000_000);

        let read_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_fetch".to_string(),
                arguments: map_args(json!({
                    "pool": "events",
                    "since": 1,
                    "tags": ["missing"]
                })),
            })
            .expect("read");
        let structured = read_result.structured_content.expect("structured");
        assert_eq!(structured["messages"], json!([]));
        assert_eq!(structured["next_seq"], json!(4));
        assert!(matches!(
            structured["ended"].as_str(),
            Some("limit" | "complete")
        ));
    }

    #[test]
    fn plasmite_fetch_reports_when_cursor_fell_behind_retention() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        let pool_ref = PoolRef::name("events");
        handler
            .client
            .create_pool(&pool_ref, PoolOptions::new(DEFAULT_POOL_SIZE))
            .expect("create pool");
        let mut opened = handler
            .client
            .open_pool(&pool_ref, false)
            .expect("open pool");
        let padding = "x".repeat(32 * 1024);
        for idx in 0..40_u64 {
            opened
                .append_json(
                    &json!({"value": idx, "padding": padding.as_str()}),
                    &[],
                    AppendOptions::new(1_700_000_000_000_000_000 + idx, Durability::Fast),
                )
                .expect("append");
        }
        let oldest = opened
            .info()
            .expect("info")
            .bounds
            .oldest_seq
            .expect("oldest");
        assert!(oldest > 1, "test pool must overwrite early messages");

        let read_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_fetch".to_string(),
                arguments: map_args(json!({
                    "pool": "events",
                    "since": 1,
                    "count": 1
                })),
            })
            .expect("read");
        assert!(read_result.is_error);
        let structured = read_result.structured_content.expect("structured");
        assert_eq!(structured["error_kind"], json!("RetentionGap"));
        assert_eq!(structured["missing"], json!(1));
        assert_eq!(structured["next_seq"], json!(oldest));
    }

    #[test]
    fn a_gap_during_a_batch_returns_the_prefix_and_resume_reports_the_gap() {
        use crate::api::ReadStatus;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gap.plasmite");
        let mut writer = Pool::create(&path, PoolOptions::new(65536)).unwrap();
        writer
            .append_json_now(&json!({"first": true}), &[], Durability::Fast)
            .unwrap();
        let pool = Pool::open(&path).unwrap();
        let mut reader = pool.follow_lite3(Read::since(Since::Oldest)).unwrap();
        let batch = super::collect_batch(20, std::time::Duration::ZERO, None, |wait| {
            let result = reader.poll_selected(wait, |frame, _| Ok(Some(Some(json!(frame.seq)))));
            if matches!(result, Ok(ReadStatus::Message(_))) {
                let large = json!({"padding": "x".repeat(4096)});
                for _ in 0..40 {
                    writer
                        .append_json_now(&large, &[], Durability::Fast)
                        .unwrap();
                }
            }
            (result, reader.position().0)
        })
        .unwrap();
        assert_eq!(batch.messages, vec![json!(1)]);
        assert_eq!(batch.next_seq, Some(2));
        assert_eq!(batch.ended, "limit");
        let gap = pool.follow_lite3(Read::since(Since::Seq(2))).err().unwrap();
        assert_eq!(gap.missing(), Some(2));
        assert_eq!(gap.next_seq(), writer.info().unwrap().bounds.oldest_seq);
    }

    #[test]
    fn a_decode_error_during_a_batch_preserves_the_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(65536)).unwrap();
        pool.append_json_now(&json!({"first": true}), &[], Durability::Fast)
            .unwrap();
        let invalid =
            crate::core::lite3::Lite3Buf::from_json_str(r#"{"meta":{"tags":[]},"data":[]}"#)
                .unwrap();
        pool.append_with_options(invalid.as_slice(), AppendOptions::new(10, Durability::Fast))
            .unwrap();
        let batch =
            super::read_batch(&pool, Read::new(), false, 20, std::time::Duration::ZERO).unwrap();
        assert_eq!(batch.messages.len(), 1);
        assert_eq!(batch.next_seq, Some(2));
        let error = super::read_batch(
            &pool,
            Read::since(Since::Seq(2)),
            false,
            20,
            std::time::Duration::ZERO,
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), ErrorKind::Corrupt);
    }

    #[test]
    fn a_filter_that_matches_nothing_still_has_a_scan_limit() {
        use crate::api::ReadStatus;
        let mut next = 1;
        let batch = super::collect_batch(20, std::time::Duration::ZERO, None, |_| {
            next += 1;
            (Ok(ReadStatus::Message(None)), Some(next))
        })
        .unwrap();
        assert!(batch.messages.is_empty());
        assert_eq!(batch.next_seq, Some(super::MAX_EXAMINED_COUNT as u64 + 1));
        assert_eq!(batch.ended, "limit");
    }

    #[test]
    fn plasmite_fetch_defaults_to_first_twenty_messages_in_ascending_order() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        seed_pool_with_messages(&mut handler, "events", 30, 1_700_000_000_000_000_000);

        let read_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_fetch".to_string(),
                arguments: map_args(json!({"pool":"events"})),
            })
            .expect("read");
        assert!(!read_result.is_error);
        let messages = read_messages_from_result(&read_result);
        assert_eq!(messages.len(), 20);
        assert_eq!(
            messages.first().and_then(|value| value["seq"].as_u64()),
            Some(1)
        );
        assert_eq!(
            messages.last().and_then(|value| value["seq"].as_u64()),
            Some(20)
        );
    }

    #[test]
    fn plasmite_fetch_since_seq_returns_ascending_forward_window() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        seed_pool_with_messages(&mut handler, "events", 10, 1_700_000_000_000_000_000);

        let read_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_fetch".to_string(),
                arguments: map_args(json!({
                    "pool":"events",
                    "since": 5,
                    "count": 3
                })),
            })
            .expect("read");
        assert!(!read_result.is_error);
        let messages = read_messages_from_result(&read_result);
        let seqs = messages
            .iter()
            .map(|message| message["seq"].as_u64().expect("seq"))
            .collect::<Vec<_>>();
        assert_eq!(seqs, vec![5, 6, 7]);
        let structured = read_result.structured_content.expect("structured");
        assert_eq!(structured["next_seq"], json!(8));
    }

    #[test]
    fn plasmite_fetch_accepts_time_start() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        let base_ns = parse_rfc3339_ns("2026-01-01T00:00:00Z").expect("base");
        seed_pool_with_messages(&mut handler, "events", 8, base_ns);

        let read_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_fetch".to_string(),
                arguments: map_args(json!({
                    "pool":"events",
                    "since": "2026-01-01T00:00:03Z",
                    "count": 3
                })),
            })
            .expect("read");
        assert!(!read_result.is_error);
        let seqs = read_messages_from_result(&read_result)
            .iter()
            .map(|message| message["seq"].as_u64().expect("seq"))
            .collect::<Vec<_>>();
        assert_eq!(seqs, vec![4, 5, 6]);
    }

    #[test]
    fn plasmite_fetch_rejects_count_outside_range() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        seed_pool_with_messages(&mut handler, "events", 1, 1_700_000_000_000_000_000);

        for count in [0, 201] {
            let read_result = handler
                .call_tool(ToolCallRequest {
                    name: "plasmite_fetch".to_string(),
                    arguments: map_args(json!({
                        "pool":"events",
                        "count": count
                    })),
                })
                .expect("read");
            assert!(read_result.is_error);
            assert_eq!(
                read_result
                    .structured_content
                    .as_ref()
                    .and_then(|value| value.get("field"))
                    .and_then(Value::as_str),
                Some("count")
            );
        }
    }

    #[test]
    fn resources_list_maps_each_pool_to_plasmite_uri() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        seed_pool_with_messages(&mut handler, "alpha", 1, 1_700_000_000_000_000_000);
        seed_pool_with_messages(&mut handler, "beta", 1, 1_700_000_000_100_000_000);

        let resources = handler.list_resources().expect("resources");
        let uris = resources
            .iter()
            .map(|resource| resource.uri.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            uris,
            vec![
                "plasmite:///pools/alpha".to_string(),
                "plasmite:///pools/beta".to_string()
            ]
        );
        assert!(
            resources
                .iter()
                .all(|resource| resource.mime_type.as_deref() == Some("application/json"))
        );
        assert!(resources.iter().all(|resource| {
            resource
                .description
                .as_deref()
                .is_some_and(|description| description.contains("reading returns up to 20"))
        }));
    }

    #[test]
    fn resources_read_returns_text_json_with_cursor() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        seed_pool_with_messages(&mut handler, "events", 3, 1_700_000_000_000_000_000);

        let read_result = handler
            .read_resource(ResourceReadRequest {
                uri: "plasmite:///pools/events".to_string(),
            })
            .expect("read resource");
        assert_eq!(read_result.contents.len(), 1);
        let content = &read_result.contents[0];
        assert_eq!(content.uri, "plasmite:///pools/events");
        assert_eq!(content.mime_type.as_deref(), Some("application/json"));
        let payload = serde_json::from_str::<Value>(content.text.as_deref().expect("text payload"))
            .expect("valid json");
        let messages = payload["messages"].as_array().expect("messages");
        let seqs = messages
            .iter()
            .map(|message| message["seq"].as_u64().expect("seq"))
            .collect::<Vec<_>>();
        assert_eq!(seqs, vec![1, 2, 3]);
        let fetched = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_fetch".into(),
                arguments: map_args(json!({"pool": "events", "last": 20})),
            })
            .unwrap();
        assert_eq!(Some(&payload), fetched.structured_content.as_ref());
        assert_eq!(fetched.structured_content.unwrap()["next_seq"], json!(4));
    }

    #[test]
    fn resources_read_rejects_host_qualified_uri() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        seed_pool_with_messages(&mut handler, "events", 1, 1_700_000_000_000_000_000);

        let err = handler
            .read_resource(ResourceReadRequest {
                uri: "plasmite://localhost/pools/events".to_string(),
            })
            .expect_err("expected invalid resource URI");
        assert_eq!(err.code, INVALID_PARAMS_CODE);
        assert_eq!(
            err.message,
            "resource uri must be in plasmite:///pools/{name} format"
        );
    }
}
