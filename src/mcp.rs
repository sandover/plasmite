//! Purpose: Provide a transport-agnostic MCP JSON-RPC core for Plasmite.
//! Key exports: `McpDispatcher`, `McpHandler`, request/response envelopes.
//! Role: Shared protocol adapter used by stdio and HTTP transports.
//! Invariants: JSON-RPC envelopes stay stable and method routing is deterministic.
//! Invariants: Unknown methods and malformed request shapes map to protocol errors.
//! Invariants: Tool execution failures can be returned as successful `result.isError`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::api::{
    Durability, Error, ErrorKind, LocalClient, PoolApiExt, PoolInfo, PoolOptions, PoolRef,
};

const JSON_RPC_VERSION: &str = "2.0";
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const PARSE_ERROR_CODE: i32 = -32700;
const INVALID_REQUEST_CODE: i32 = -32600;
const METHOD_NOT_FOUND_CODE: i32 = -32601;
const INVALID_PARAMS_CODE: i32 = -32602;
const INTERNAL_ERROR_CODE: i32 = -32603;
const DEFAULT_POOL_SIZE_BYTES: u64 = 1024 * 1024;
const DEFAULT_READ_COUNT: usize = 20;
const MAX_READ_COUNT: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    pub protocol_version: String,
}

impl Default for ServerMetadata {
    fn default() -> Self {
        Self {
            name: "plasmite".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerCapabilities {
    pub tools: ToolsCapability,
    pub resources: ResourcesCapability,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourcesCapability {
    pub subscribe: bool,
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
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

pub trait McpHandler {
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
        match parse_jsonrpc_request(value) {
            Ok(request) => self.dispatch_request(request),
            Err(response) => DispatchOutcome::Response(*response),
        }
    }

    pub fn dispatch_request(&mut self, request: JsonRpcRequest) -> DispatchOutcome {
        let id = request.id.clone();
        let route_result = self.route_method(request);
        match id {
            Some(response_id) => match route_result {
                Ok(result) => {
                    DispatchOutcome::Response(JsonRpcResponse::success(response_id, result))
                }
                Err(error) => DispatchOutcome::Response(JsonRpcResponse::error(response_id, error)),
            },
            None => DispatchOutcome::NoResponse,
        }
    }

    fn route_method(&mut self, request: JsonRpcRequest) -> Result<Value, JsonRpcError> {
        match request.method.as_str() {
            "initialize" => {
                ensure_object_or_absent(request.params.as_ref())?;
                to_value(self.initialize_result())
            }
            "notifications/initialized" => {
                ensure_object_or_absent(request.params.as_ref())?;
                Ok(json!({}))
            }
            "ping" => {
                ensure_object_or_absent(request.params.as_ref())?;
                Ok(json!({}))
            }
            "tools/list" => {
                ensure_object_or_absent(request.params.as_ref())?;
                let tools = self.handler.list_tools()?;
                Ok(json!({ "tools": tools }))
            }
            "tools/call" => {
                let params = require_object_params(
                    request.params.as_ref(),
                    "tools/call requires object params",
                )?;
                let tool_request = parse_tool_call_params(params)?;
                let result = self.handler.call_tool(tool_request)?;
                to_value(result)
            }
            "resources/list" => {
                ensure_object_or_absent(request.params.as_ref())?;
                let resources = self.handler.list_resources()?;
                Ok(json!({ "resources": resources }))
            }
            "resources/read" => {
                let params = require_object_params(
                    request.params.as_ref(),
                    "resources/read requires object params",
                )?;
                let read_request = parse_resource_read_params(params)?;
                let result = self.handler.read_resource(read_request)?;
                to_value(result)
            }
            _ => Err(JsonRpcError::method_not_found(format!(
                "method not found: {}",
                request.method
            ))),
        }
    }

    fn initialize_result(&self) -> InitializeResult {
        InitializeResult {
            protocol_version: self.metadata.protocol_version.clone(),
            capabilities: ServerCapabilities {
                tools: ToolsCapability {
                    list_changed: false,
                },
                resources: ResourcesCapability {
                    subscribe: false,
                    list_changed: false,
                },
            },
            server_info: ServerInfo {
                name: self.metadata.name.clone(),
                version: self.metadata.version.clone(),
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct PlasmiteMcpHandler {
    client: LocalClient,
}

impl PlasmiteMcpHandler {
    pub fn new(pool_dir: impl Into<PathBuf>) -> Self {
        Self::with_client(LocalClient::new().with_pool_dir(pool_dir))
    }

    pub fn with_client(client: LocalClient) -> Self {
        Self { client }
    }

    fn tool_pool_list(&self) -> ToolCallResult {
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
        ToolCallResult::success_with_structured(
            format!("Listed {} pool(s).", pools_json.len()),
            json!({ "pools": pools_json }),
        )
    }

    fn tool_pool_create(&self, args: &Map<String, Value>) -> ToolCallResult {
        let name = match required_string_arg(args, "name") {
            Ok(name) => name,
            Err(result) => return invalid_argument_result("plasmite_pool_create", "name", result),
        };
        let size = match optional_u64_arg(args, "size") {
            Ok(Some(size)) => size,
            Ok(None) => DEFAULT_POOL_SIZE_BYTES,
            Err(result) => return invalid_argument_result("plasmite_pool_create", "size", result),
        };

        let pool_ref = PoolRef::name(name.clone());
        let info = match self.client.create_pool(&pool_ref, PoolOptions::new(size)) {
            Ok(info) => info,
            Err(err) => return api_error_tool_result("plasmite_pool_create", err),
        };

        ToolCallResult::success_with_structured(
            format!("Created pool `{name}`."),
            json!({ "pool": pool_info_json_value(&name, &info) }),
        )
    }

    fn tool_pool_info(&self, args: &Map<String, Value>) -> ToolCallResult {
        let pool = match required_string_arg(args, "pool") {
            Ok(name) => name,
            Err(result) => return invalid_argument_result("plasmite_pool_info", "pool", result),
        };
        let pool_ref = PoolRef::name(pool.clone());
        let info = match self.client.pool_info(&pool_ref) {
            Ok(info) => info,
            Err(err) => return api_error_tool_result("plasmite_pool_info", err),
        };
        ToolCallResult::success_with_structured(
            format!("Fetched metadata for pool `{pool}`."),
            json!({ "pool": pool_info_json_value(&pool, &info) }),
        )
    }

    fn tool_pool_delete(&self, args: &Map<String, Value>) -> ToolCallResult {
        let pool = match required_string_arg(args, "pool") {
            Ok(name) => name,
            Err(result) => return invalid_argument_result("plasmite_pool_delete", "pool", result),
        };
        let pool_ref = PoolRef::name(pool.clone());
        if let Err(err) = self.client.delete_pool(&pool_ref) {
            return api_error_tool_result("plasmite_pool_delete", err);
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
        let mut opened = match self.client.open_pool(&pool_ref) {
            Ok(pool) => pool,
            Err(err) if create && err.kind() == ErrorKind::NotFound => {
                if let Err(create_err) = self
                    .client
                    .create_pool(&pool_ref, PoolOptions::new(DEFAULT_POOL_SIZE_BYTES))
                {
                    return api_error_tool_result("plasmite_feed", create_err);
                }
                match self.client.open_pool(&pool_ref) {
                    Ok(pool) => pool,
                    Err(open_err) => return api_error_tool_result("plasmite_feed", open_err),
                }
            }
            Err(err) => return api_error_tool_result("plasmite_feed", err),
        };

        let message = match opened.append_json_now(&data, &tags, Durability::Fast) {
            Ok(message) => message,
            Err(err) => return api_error_tool_result("plasmite_feed", err),
        };

        ToolCallResult::success_with_structured(
            format!("Appended message {} to `{pool}`.", message.seq),
            json!({ "message": message_json_value(&message) }),
        )
    }

    fn tool_fetch(&self, args: &Map<String, Value>) -> ToolCallResult {
        let pool = match required_string_arg(args, "pool") {
            Ok(name) => name,
            Err(result) => return invalid_argument_result("plasmite_fetch", "pool", result),
        };
        let seq = match required_u64_arg(args, "seq") {
            Ok(seq) => seq,
            Err(result) => return invalid_argument_result("plasmite_fetch", "seq", result),
        };
        let pool_ref = PoolRef::name(pool.clone());
        let opened = match self.client.open_pool(&pool_ref) {
            Ok(pool) => pool,
            Err(err) => return api_error_tool_result("plasmite_fetch", err),
        };
        let message = match opened.get_message(seq) {
            Ok(message) => message,
            Err(err) => return api_error_tool_result("plasmite_fetch", err),
        };
        ToolCallResult::success_with_structured(
            format!("Fetched sequence {seq} from `{pool}`."),
            json!({ "message": message_json_value(&message) }),
        )
    }

    fn tool_read(&self, args: &Map<String, Value>) -> ToolCallResult {
        let pool = match required_string_arg(args, "pool") {
            Ok(name) => name,
            Err(result) => return invalid_argument_result("plasmite_read", "pool", result),
        };
        let count = match optional_usize_arg(args, "count") {
            Ok(Some(value)) => value,
            Ok(None) => DEFAULT_READ_COUNT,
            Err(result) => return invalid_argument_result("plasmite_read", "count", result),
        };
        if count > MAX_READ_COUNT {
            return invalid_argument_result(
                "plasmite_read",
                "count",
                format!("count must be <= {MAX_READ_COUNT}"),
            );
        }
        let after_seq = match optional_u64_arg(args, "after_seq") {
            Ok(value) => value,
            Err(result) => return invalid_argument_result("plasmite_read", "after_seq", result),
        };
        let since = match optional_string_arg(args, "since") {
            Ok(value) => value,
            Err(result) => return invalid_argument_result("plasmite_read", "since", result),
        };
        let since_ns = match since {
            Some(since) => match parse_since_ns(&since, now_unix_ns()) {
                Ok(value) => Some(value),
                Err(err) => return invalid_argument_result("plasmite_read", "since", err),
            },
            None => None,
        };
        let tags = match optional_string_array_arg(args, "tags") {
            Ok(Some(tags)) => tags,
            Ok(None) => Vec::new(),
            Err(result) => return invalid_argument_result("plasmite_read", "tags", result),
        };
        if args.contains_key("where") {
            return invalid_argument_result(
                "plasmite_read",
                "where",
                "where filtering is not implemented in experimental v1",
            );
        }

        let pool_ref = PoolRef::name(pool.clone());
        let opened = match self.client.open_pool(&pool_ref) {
            Ok(pool) => pool,
            Err(err) => return api_error_tool_result("plasmite_read", err),
        };
        let info = match opened.info() {
            Ok(info) => info,
            Err(err) => return api_error_tool_result("plasmite_read", err),
        };
        let messages =
            match read_messages_for_tool(&opened, &info, count, after_seq, since_ns, &tags) {
                Ok(messages) => messages,
                Err(err) => return api_error_tool_result("plasmite_read", *err),
            };
        let next_after_seq = messages
            .last()
            .and_then(|message| message.get("seq"))
            .and_then(Value::as_u64)
            .or(after_seq);

        ToolCallResult::success_with_structured(
            format!("Read {} message(s) from `{pool}`.", messages.len()),
            json!({
                "messages": messages,
                "next_after_seq": next_after_seq,
            }),
        )
    }
}

impl Default for PlasmiteMcpHandler {
    fn default() -> Self {
        Self::with_client(LocalClient::new())
    }
}

impl McpHandler for PlasmiteMcpHandler {
    fn list_tools(&mut self) -> Result<Vec<McpTool>, JsonRpcError> {
        Ok(vec![
            McpTool {
                name: "plasmite_pool_list".to_string(),
                description: "List all pools in the pool directory.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            McpTool {
                name: "plasmite_pool_create".to_string(),
                description: "Create a new pool. Returns pool info on success.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "name": {"type":"string","description":"Pool name"},
                        "size": {"type":"integer","description":"Pool size in bytes (default: 1048576)"}
                    },
                    "required": ["name"]
                }),
            },
            McpTool {
                name: "plasmite_pool_info".to_string(),
                description: "Get metadata and metrics for a pool.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pool": {"type":"string","description":"Pool name"}
                    },
                    "required": ["pool"]
                }),
            },
            McpTool {
                name: "plasmite_pool_delete".to_string(),
                description: "Delete a pool.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pool": {"type":"string","description":"Pool name"}
                    },
                    "required": ["pool"]
                }),
            },
            McpTool {
                name: "plasmite_feed".to_string(),
                description: "Append a JSON message to a pool. Returns the committed message envelope (seq, time, meta).".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pool": {"type":"string","description":"Pool name"},
                        "data": {"description":"JSON message payload (object, array, string, number, etc.)"},
                        "tags": {"type":"array","items":{"type":"string"},"description":"Optional tags for filtering"},
                        "create": {"type":"boolean","description":"Create the pool if it doesn't exist (default: false)"}
                    },
                    "required": ["pool", "data"]
                }),
            },
            McpTool {
                name: "plasmite_fetch".to_string(),
                description: "Fetch a single message by sequence number.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pool": {"type":"string","description":"Pool name"},
                        "seq": {"type":"integer","description":"Sequence number"}
                    },
                    "required": ["pool", "seq"]
                }),
            },
            McpTool {
                name: "plasmite_read".to_string(),
                description: "Read messages from a pool. Returns up to `count` messages in ascending sequence order. Without `after_seq`, this returns the last `count` matching messages (still ascending). Use `since` for a time window, or `after_seq` to resume from a known position.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pool": {"type":"string","description":"Pool name"},
                        "count": {"type":"integer","description":"Max messages to return (default: 20, max: 200)"},
                        "after_seq": {"type":"integer","description":"Return messages after this sequence number (for pagination/resumption)"},
                        "since": {"type":"string","description":"Time window, e.g. '5m', '1h', '2024-01-15T00:00:00Z'"},
                        "tags": {"type":"array","items":{"type":"string"},"description":"Filter by tags"},
                        "where": {"type":"string","description":"jq predicate for filtering, e.g. '.data.status == \"done\"'"}
                    },
                    "required": ["pool"]
                }),
            },
        ])
    }

    fn call_tool(&mut self, request: ToolCallRequest) -> Result<ToolCallResult, JsonRpcError> {
        let args = request.arguments;
        let result = match request.name.as_str() {
            "plasmite_pool_list" => self.tool_pool_list(),
            "plasmite_pool_create" => self.tool_pool_create(&args),
            "plasmite_pool_info" => self.tool_pool_info(&args),
            "plasmite_pool_delete" => self.tool_pool_delete(&args),
            "plasmite_feed" => self.tool_feed(&args),
            "plasmite_fetch" => self.tool_fetch(&args),
            "plasmite_read" => self.tool_read(&args),
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
                        "Plasmite pool: {name} ({}, {} bytes)",
                        pool_bounds_label(&info),
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
            .open_pool(&pool_ref)
            .map_err(api_error_jsonrpc)?;
        let info = opened.info().map_err(api_error_jsonrpc)?;
        let messages = read_messages_for_tool(&opened, &info, DEFAULT_READ_COUNT, None, None, &[])
            .map_err(|err| api_error_jsonrpc(*err))?;
        let next_after_seq = messages
            .last()
            .and_then(|message| message.get("seq"))
            .and_then(Value::as_u64);
        let payload = json!({
            "messages": messages,
            "next_after_seq": next_after_seq,
        });
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

fn require_object_params<'a>(
    params: Option<&'a Value>,
    message: &'static str,
) -> Result<&'a Map<String, Value>, JsonRpcError> {
    match params {
        Some(Value::Object(map)) => Ok(map),
        _ => Err(JsonRpcError::invalid_params(message)),
    }
}

fn ensure_object_or_absent(params: Option<&Value>) -> Result<(), JsonRpcError> {
    match params {
        None | Some(Value::Null) | Some(Value::Object(_)) => Ok(()),
        _ => Err(JsonRpcError::invalid_params(
            "params must be an object when provided",
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

fn optional_string_arg(args: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn required_value_arg(args: &Map<String, Value>, key: &str) -> Result<Value, String> {
    args.get(key)
        .cloned()
        .ok_or_else(|| format!("missing required `{key}` argument"))
}

fn required_u64_arg(args: &Map<String, Value>, key: &str) -> Result<u64, String> {
    optional_u64_arg(args, key)?.ok_or_else(|| format!("missing required `{key}` argument"))
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
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
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
    let mut map = Map::new();
    map.insert("name".to_string(), json!(pool_ref));
    map.insert("path".to_string(), json!(info.path.display().to_string()));
    map.insert("file_size".to_string(), json!(info.file_size));
    map.insert("index_offset".to_string(), json!(info.index_offset));
    map.insert("index_capacity".to_string(), json!(info.index_capacity));
    map.insert("index_size_bytes".to_string(), json!(info.index_size_bytes));
    map.insert("ring_offset".to_string(), json!(info.ring_offset));
    map.insert("ring_size".to_string(), json!(info.ring_size));
    map.insert("bounds".to_string(), pool_bounds_json_value(info));
    if let Some(metrics) = &info.metrics {
        map.insert(
            "metrics".to_string(),
            json!({
                "message_count": metrics.message_count,
                "seq_span": metrics.seq_span,
                "utilization": {
                    "used_bytes": metrics.utilization.used_bytes,
                    "free_bytes": metrics.utilization.free_bytes,
                    "used_percent": (metrics.utilization.used_percent_hundredths as f64) / 100.0
                },
                "age": {
                    "oldest_time": metrics.age.oldest_time,
                    "newest_time": metrics.age.newest_time,
                    "oldest_age_ms": metrics.age.oldest_age_ms,
                    "newest_age_ms": metrics.age.newest_age_ms
                }
            }),
        );
    }
    Value::Object(map)
}

fn pool_bounds_json_value(info: &PoolInfo) -> Value {
    let mut map = Map::new();
    if let Some(oldest) = info.bounds.oldest_seq {
        map.insert("oldest".to_string(), json!(oldest));
    }
    if let Some(newest) = info.bounds.newest_seq {
        map.insert("newest".to_string(), json!(newest));
    }
    Value::Object(map)
}

fn message_json_value(message: &crate::api::Message) -> Value {
    json!({
        "seq": message.seq,
        "time": message.time.clone(),
        "meta": {
            "tags": message.meta.tags.clone(),
        },
        "data": message.data.clone(),
    })
}

fn read_messages_for_tool(
    pool: &crate::api::Pool,
    info: &PoolInfo,
    count: usize,
    after_seq: Option<u64>,
    since_ns: Option<u64>,
    required_tags: &[String],
) -> Result<Vec<Value>, Box<Error>> {
    let (Some(oldest), Some(newest)) = (info.bounds.oldest_seq, info.bounds.newest_seq) else {
        return Ok(Vec::new());
    };
    if count == 0 {
        return Ok(Vec::new());
    }

    let start_seq = match after_seq {
        Some(after_seq) => oldest.max(after_seq.saturating_add(1)),
        None => oldest,
    };
    if start_seq > newest {
        return Ok(Vec::new());
    }

    let mut messages = Vec::new();
    for seq in start_seq..=newest {
        let message = match pool.get_message(seq) {
            Ok(message) => message,
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => return Err(Box::new(err)),
        };
        if !message_has_tags(&message.meta.tags, required_tags) {
            continue;
        }
        if let Some(since_ns) = since_ns {
            let message_ns = parse_rfc3339_ns(&message.time)
                .map_err(|parse_err| {
                    Error::new(ErrorKind::Corrupt)
                        .with_message("stored message has invalid timestamp")
                        .with_source(parse_err)
                })
                .map_err(Box::new)?;
            if message_ns < since_ns {
                continue;
            }
        }

        if after_seq.is_some() {
            messages.push(message_json_value(&message));
            if messages.len() >= count {
                break;
            }
            continue;
        }

        messages.push(message_json_value(&message));
        if messages.len() > count {
            messages.remove(0);
        }
    }
    Ok(messages)
}

fn message_has_tags(message_tags: &[String], required_tags: &[String]) -> bool {
    required_tags
        .iter()
        .all(|required| message_tags.iter().any(|tag| tag == required))
}

fn parse_since_ns(input: &str, now_ns: u64) -> Result<u64, String> {
    if let Some(duration_ns) = parse_relative_since_ns(input) {
        return Ok(now_ns.saturating_sub(duration_ns));
    }
    parse_rfc3339_ns(input).map_err(|_| {
        "since must be RFC 3339 (2026-02-02T23:45:00Z) or relative like 5m".to_string()
    })
}

fn parse_relative_since_ns(input: &str) -> Option<u64> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (digits, unit) = trimmed.split_at(trimmed.len().saturating_sub(1));
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let value: u64 = digits.parse().ok()?;
    let seconds = match unit {
        "s" | "S" => value,
        "m" | "M" => value.saturating_mul(60),
        "h" | "H" => value.saturating_mul(60 * 60),
        "d" | "D" => value.saturating_mul(60 * 60 * 24),
        _ => return None,
    };
    Some(seconds.saturating_mul(1_000_000_000))
}

fn parse_rfc3339_ns(value: &str) -> Result<u64, time::error::Parse> {
    let timestamp =
        time::OffsetDateTime::parse(value.trim(), &time::format_description::well_known::Rfc3339)?;
    Ok(timestamp.unix_timestamp_nanos().max(0) as u64)
}

fn now_unix_ns() -> u64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0));
    duration
        .as_secs()
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::from(duration.subsec_nanos()))
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

fn api_error_tool_result(tool: &str, err: Error) -> ToolCallResult {
    let mut text = err
        .message()
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("{:?}", err.kind()));
    if let Some(hint) = err.hint() {
        text = format!("{text}. {hint}");
    }

    let mut structured = Map::new();
    structured.insert("tool".to_string(), json!(tool));
    structured.insert("error_kind".to_string(), json!(format!("{:?}", err.kind())));
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

    ToolCallResult::execution_error_with_structured(text, Some(Value::Object(structured)))
}

fn api_error_jsonrpc(err: Error) -> JsonRpcError {
    let mut rpc_error = JsonRpcError::new(
        -32000,
        err.message()
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("{:?}", err.kind())),
    );
    let mut data = Map::new();
    data.insert("error_kind".to_string(), json!(format!("{:?}", err.kind())));
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
    if !data.is_empty() {
        rpc_error.data = Some(Value::Object(data));
    }
    rpc_error
}

fn pool_bounds_label(info: &PoolInfo) -> String {
    match (info.bounds.oldest_seq, info.bounds.newest_seq) {
        (Some(oldest), Some(newest)) => format!("seq {oldest}-{newest}"),
        _ => "empty".to_string(),
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
    use crate::api::AppendOptions;

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
        if let Some(params) = params {
            object
                .as_object_mut()
                .expect("object")
                .insert("params".to_string(), params);
        }
        object
    }

    fn expect_response(outcome: DispatchOutcome) -> JsonRpcResponse {
        match outcome {
            DispatchOutcome::Response(response) => response,
            DispatchOutcome::NoResponse => panic!("expected response"),
        }
    }

    #[test]
    fn initialize_routes_with_capabilities() {
        let handler = StubHandler::default();
        let mut dispatcher = McpDispatcher::new(handler);
        let response = expect_response(dispatcher.dispatch_value(request(
            JsonRpcId::Number(1),
            "initialize",
            Some(json!({})),
        )));
        let result = response.result.expect("result");
        assert_eq!(response.error, None);
        assert_eq!(result["protocolVersion"], json!("2025-11-25"));
        assert_eq!(result["capabilities"]["tools"]["listChanged"], json!(false));
        assert_eq!(
            result["capabilities"]["resources"]["listChanged"],
            json!(false)
        );
        assert_eq!(
            result["capabilities"]["resources"]["subscribe"],
            json!(false)
        );
        assert_eq!(result["serverInfo"]["name"], json!("plasmite"));
    }

    #[test]
    fn initialized_notification_returns_no_response() {
        let handler = StubHandler::default();
        let mut dispatcher = McpDispatcher::new(handler);
        let outcome = dispatcher.dispatch_value(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {},
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
        assert_eq!(response.result, Some(json!({})));
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
        assert_eq!(
            list_response.result.expect("tools list result")["tools"]
                .as_array()
                .expect("tools"),
            &vec![json!({
                "name":"plasmite_pool_list",
                "description":"List pools",
                "inputSchema":{"type":"object","properties":{}}
            })]
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
        assert_eq!(
            call_response.result.expect("tools call result")["content"][0]["text"],
            json!("ok")
        );

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
                "name": "plasmite_read",
                "arguments": {"pool":"missing"}
            })),
        )));
        assert_eq!(response.error, None);
        let result = response.result.expect("result");
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["content"][0]["text"], json!("pool not found"));
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
        let mut opened = handler.client.open_pool(&pool_ref).expect("open pool");
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
                    "seq": seq
                })),
            })
            .expect("fetch");
        assert!(!fetch_result.is_error);
        assert_eq!(
            fetch_result
                .structured_content
                .as_ref()
                .expect("structured")
                .get("message")
                .and_then(|value| value.get("data"))
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
    fn plasmite_read_defaults_to_last_twenty_messages_in_ascending_order() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        seed_pool_with_messages(&mut handler, "events", 30, 1_700_000_000_000_000_000);

        let read_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_read".to_string(),
                arguments: map_args(json!({"pool":"events"})),
            })
            .expect("read");
        assert!(!read_result.is_error);
        let messages = read_messages_from_result(&read_result);
        assert_eq!(messages.len(), 20);
        assert_eq!(
            messages.first().and_then(|value| value["seq"].as_u64()),
            Some(11)
        );
        assert_eq!(
            messages.last().and_then(|value| value["seq"].as_u64()),
            Some(30)
        );
    }

    #[test]
    fn plasmite_read_after_seq_returns_ascending_forward_window() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        seed_pool_with_messages(&mut handler, "events", 10, 1_700_000_000_000_000_000);

        let read_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_read".to_string(),
                arguments: map_args(json!({
                    "pool":"events",
                    "after_seq": 4,
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
    }

    #[test]
    fn plasmite_read_since_and_after_seq_intersect() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        let base_ns = parse_rfc3339_ns("2026-01-01T00:00:00Z").expect("base");
        seed_pool_with_messages(&mut handler, "events", 8, base_ns);

        let read_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_read".to_string(),
                arguments: map_args(json!({
                    "pool":"events",
                    "after_seq": 2,
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
    fn plasmite_read_rejects_count_above_max() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut handler = PlasmiteMcpHandler::new(tmp.path());
        seed_pool_with_messages(&mut handler, "events", 1, 1_700_000_000_000_000_000);

        let read_result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_read".to_string(),
                arguments: map_args(json!({
                    "pool":"events",
                    "count": 201
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
        assert_eq!(payload["next_after_seq"], json!(3));
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
