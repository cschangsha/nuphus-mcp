//! JSON-RPC 2.0 wire types + MCP error codes.
//!
//! Protocol mirror reference: `src/mcp/client.rs` (Nuphus main crate's MCP stdio client).
//! The client sends one-line JSON requests and the server responds line by line.

use serde::Serialize;
use serde_json::Value;

/// JSON-RPC version number
pub const JSONRPC_VERSION: &str = "2.0";

/// MCP protocol version (matches the initialize request in client.rs)
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// MCP JSON-RPC error codes
pub mod codes {
    /// Parse error — request is not valid JSON
    pub const PARSE_ERROR: i32 = -32700;
    /// Invalid Request — structurally invalid
    pub const INVALID_REQUEST: i32 = -32600;
    /// Method not found — unknown method
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// Invalid params — invalid parameters (including unknown tools in tools/call)
    pub const INVALID_PARAMS: i32 = -32602;
    /// Internal error
    pub const INTERNAL_ERROR: i32 = -32603;
    /// Server not initialized — business request received before the initialize handshake
    pub const SERVER_NOT_INITIALIZED: i32 = -32000;
    // Note: tool execution errors are NOT mapped to a JSON-RPC error code. Per MCP spec,
    // a tools/call semantic failure is returned as a successful response with
    // `content.isError: true` (see tools::execute → ToolOutput::failure). The only
    // JSON-RPC error a tools/call can produce here is INVALID_PARAMS for an unknown tool name.
}

/// Inbound JSON-RPC request (including notifications).
///
/// `id == None` means the `id` member is ABSENT — a notification that must not
/// be answered. An explicitly present `"id": null` is `Some(Value::Null)` and
/// MUST be answered: a client waiting on that id would otherwise hang forever.
#[derive(Debug, Clone)]
pub struct Request {
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

impl Request {
    /// Parse and structurally validate one inbound JSON-RPC line.
    ///
    /// Error mapping per JSON-RPC 2.0:
    /// - syntactically invalid JSON             → [`codes::PARSE_ERROR`] (-32700)
    /// - valid JSON but structurally not a Request → [`codes::INVALID_REQUEST`] (-32600):
    ///   not an object / `jsonrpc` missing or not `"2.0"` / `method` missing or not a string
    pub fn parse(line: &str) -> Result<Self, RpcError> {
        let value: Value = serde_json::from_str(line)
            .map_err(|e| RpcError::new(codes::PARSE_ERROR, format!("Parse error: {}", e)))?;

        let obj = value.as_object().ok_or_else(|| {
            RpcError::new(
                codes::INVALID_REQUEST,
                "Invalid Request: expected a JSON-RPC request object",
            )
        })?;

        match obj.get("jsonrpc").and_then(Value::as_str) {
            Some(JSONRPC_VERSION) => {}
            _ => {
                return Err(RpcError::new(
                    codes::INVALID_REQUEST,
                    "Invalid Request: \"jsonrpc\" must be \"2.0\"",
                ));
            }
        }

        let method = obj
            .get("method")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .ok_or_else(|| {
                RpcError::new(
                    codes::INVALID_REQUEST,
                    "Invalid Request: \"method\" must be a non-empty string",
                )
            })?;

        Ok(Self {
            // obj.get() distinguishes an absent id (notification) from an
            // explicit "id": null (a request that expects a response).
            id: obj.get("id").cloned(),
            method: method.to_string(),
            params: obj.get("params").cloned(),
        })
    }
}

/// JSON-RPC error object
#[derive(Debug, Clone, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// Outbound JSON-RPC response
#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    /// Successful response
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }

    /// Error response
    pub fn err(id: Value, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id: Some(id),
            result: None,
            error: Some(error),
        }
    }

    /// Serialize to a single-line JSON (stdio transport)
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"response serialization failed"}}"#
                .to_string()
        })
    }
}
