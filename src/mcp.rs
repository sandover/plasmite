//! Purpose: Provide a transport-agnostic MCP JSON-RPC core for Plasmite.
//! Key exports: `McpDispatcher`, `McpHandler`, request/response envelopes.
//! Role: Shared protocol adapter used by stdio and HTTP transports.
//! Invariants: JSON-RPC envelopes stay stable and method routing is deterministic.
//! Invariants: Unknown methods and malformed request shapes map to protocol errors.
//! Invariants: Tool execution failures can be returned as successful `result.isError`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

const JSON_RPC_VERSION: &str = "2.0";
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const PARSE_ERROR_CODE: i32 = -32700;
const INVALID_REQUEST_CODE: i32 = -32600;
const METHOD_NOT_FOUND_CODE: i32 = -32601;
const INVALID_PARAMS_CODE: i32 = -32602;
const INTERNAL_ERROR_CODE: i32 = -32603;

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

    pub fn execution_error_text(message: impl Into<String>) -> Self {
        Self {
            content: vec![json!({
                "type": "text",
                "text": message.into(),
            })],
            is_error: true,
            structured_content: None,
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
            Err(response) => DispatchOutcome::Response(response),
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

pub fn parse_jsonrpc_line(line: &str) -> Result<Value, JsonRpcError> {
    serde_json::from_str::<Value>(line).map_err(|_| JsonRpcError::parse_error("invalid JSON"))
}

fn parse_jsonrpc_request(value: Value) -> Result<JsonRpcRequest, JsonRpcResponse> {
    let mut object = match value {
        Value::Object(object) => object,
        _ => {
            return Err(JsonRpcResponse::error(
                JsonRpcId::Null,
                JsonRpcError::invalid_request("request must be a JSON object"),
            ));
        }
    };

    let mut id: Option<JsonRpcId> = None;
    if let Some(raw_id) = object.remove("id") {
        let parsed_id = parse_jsonrpc_id(raw_id)
            .map_err(|error| JsonRpcResponse::error(JsonRpcId::Null, error))?;
        id = Some(parsed_id);
    }
    let error_id = id.clone().unwrap_or(JsonRpcId::Null);

    let jsonrpc = object
        .remove("jsonrpc")
        .and_then(|value| value.as_str().map(ToString::to_string))
        .ok_or_else(|| {
            JsonRpcResponse::error(
                error_id.clone(),
                JsonRpcError::invalid_request("missing jsonrpc field"),
            )
        })?;
    if jsonrpc != JSON_RPC_VERSION {
        return Err(JsonRpcResponse::error(
            error_id,
            JsonRpcError::invalid_request("jsonrpc must be \"2.0\""),
        ));
    }

    let method = object
        .remove("method")
        .and_then(|value| value.as_str().map(ToString::to_string))
        .ok_or_else(|| {
            JsonRpcResponse::error(
                id.clone().unwrap_or(JsonRpcId::Null),
                JsonRpcError::invalid_request("missing method field"),
            )
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
