//! MCP server: tool registry, dispatch loop, and JSON-RPC handling.
//!
//! [`MCPServer`] runs the protocol loop over a [`Transport`]:
//!
//! 1. Wait for `initialize` from the client; respond with server info.
//! 2. Accept subsequent requests: `tools/list`, `tools/call`,
//!    `notifications/initialized`, and `ping`.
//! 3. For `tools/call`, look up the named tool in the registry, validate
//!    `arguments` against the tool's input schema (lightly), and invoke
//!    the handler.
//!
//! Errors are returned as JSON-RPC error responses with code -32603
//! (internal error) or -32602 (invalid params).

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use crate::mcp::transport::Transport;
use crate::mcp::types::{
    ContentBlock, Implementation, JSONRPCError, JSONRPCMessage, JSONRPCRequest, JSONRPCResponse,
    ListToolsResult, ToolDefinition, ToolHandler,
};

/// JSON-RPC error code: parse error.
pub const ERR_PARSE: i64 = -32700;
/// JSON-RPC error code: invalid request.
pub const ERR_INVALID_REQUEST: i64 = -32600;
/// JSON-RPC error code: method not found.
pub const ERR_METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC error code: invalid params.
pub const ERR_INVALID_PARAMS: i64 = -32602;
/// JSON-RPC error code: internal error.
pub const ERR_INTERNAL: i64 = -32603;

type ToolEntry = (ToolDefinition, Arc<dyn ToolHandler>);

/// Lightweight JSON-Schema argument validation.
///
/// Checks that every entry in the schema's `required` array is present in
/// `arguments` (and not `null`). This is intentionally minimal — it does not
/// validate types or enum constraints — but it catches the common case of a
/// missing required parameter.
fn validate_arguments(schema: &Value, args: &Value) -> Result<()> {
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    if required.is_empty() {
        return Ok(());
    }
    let obj = match args {
        Value::Object(m) => m,
        _ => return Err(Error::InvalidInput("arguments must be an object".into())),
    };
    for key in required {
        match obj.get(key) {
            Some(Value::Null) | None => {
                return Err(Error::InvalidInput(format!(
                    "missing required argument `{key}`"
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Mutable tool registry protected by a mutex.
pub struct ToolRegistry {
    tools: Mutex<HashMap<String, ToolEntry>>,
}

impl ToolRegistry {
    /// Build a fresh empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tools: Mutex::new(HashMap::new()),
        }
    }

    /// Register a tool, replacing any existing tool with the same name.
    pub async fn register(&self, def: ToolDefinition, handler: Arc<dyn ToolHandler>) {
        let mut guard = self.tools.lock().await;
        guard.insert(def.name.clone(), (def, handler));
    }

    /// Snapshot of all tool definitions, sorted alphabetically by name.
    async fn definitions(&self) -> Vec<ToolDefinition> {
        let guard = self.tools.lock().await;
        let mut defs: Vec<ToolDefinition> = guard.values().map(|(d, _)| d.clone()).collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }

    /// Look up both the tool definition and handler by name.
    async fn get_entry(&self, name: &str) -> Option<(ToolDefinition, Arc<dyn ToolHandler>)> {
        let guard = self.tools.lock().await;
        guard.get(name).cloned()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for an [`MCPServer`] that allows registering tools before serving.
pub struct ServerBuilder {
    implementation: Implementation,
    registry: Arc<ToolRegistry>,
}

impl ServerBuilder {
    /// Start a new builder with the given server identity.
    #[must_use]
    pub fn new(implementation: Implementation) -> Self {
        Self {
            implementation,
            registry: Arc::new(ToolRegistry::new()),
        }
    }

    /// Register a tool on the server being built.
    pub async fn tool(self, def: ToolDefinition, handler: Arc<dyn ToolHandler>) -> Self {
        self.registry.register(def, handler).await;
        self
    }

    /// Finalize the builder into an [`MCPServer`].
    #[must_use]
    pub fn build(self) -> MCPServer {
        MCPServer {
            implementation: Arc::new(self.implementation),
            registry: self.registry,
        }
    }
}

/// MCP server instance with a registered set of tools.
pub struct MCPServer {
    implementation: Arc<Implementation>,
    registry: Arc<ToolRegistry>,
}

impl MCPServer {
    /// Build a new server; see [`ServerBuilder`] for the fluent API.
    #[must_use]
    pub fn new(implementation: Implementation) -> Self {
        ServerBuilder::new(implementation).build()
    }

    /// Returns a reference to the tool registry (for advanced setups).
    #[must_use]
    pub fn registry(&self) -> &Arc<ToolRegistry> {
        &self.registry
    }

    /// Run the protocol loop on the given transport until EOF or error.
    ///
    /// Returns `Ok(())` on clean EOF, `Err` on transport/protocol failure.
    pub async fn serve(&self, transport: &mut dyn Transport) -> Result<()> {
        loop {
            // A JSON parse failure is a *client* error (JSON-RPC 2.0 §5.1):
            // the server MUST reply with `id: null` and code -32700, then keep
            // the connection alive. Real I/O failures still terminate the loop.
            let msg = match transport.recv().await {
                Ok(Some(m)) => m,
                Ok(None) => return Ok(()),
                // Typed parse-error variant (NEW-M8): the old
                // `Error::Internal(msg) if msg.starts_with("parse:")` relied
                // on the transport's error text staying prefixed with
                // "parse:" — brittle across refactors.
                Err(Error::JsonRpcParse(msg)) => {
                    let resp = JSONRPCMessage::Response(JSONRPCResponse {
                        jsonrpc: "2.0".to_string(),
                        id: Value::Null,
                        result: None,
                        error: Some(JSONRPCError {
                            code: ERR_PARSE,
                            message: msg,
                            data: None,
                        }),
                    });
                    transport.send(&resp).await?;
                    continue;
                }
                Err(e) => return Err(e),
            };
            let response = self.dispatch(msg).await?;
            if let Some(resp) = response {
                transport.send(&resp).await?;
            }
        }
    }

    /// Dispatch a single incoming message and optionally produce a response.
    async fn dispatch(&self, msg: JSONRPCMessage) -> Result<Option<JSONRPCMessage>> {
        match msg {
            JSONRPCMessage::Request(req) => {
                let resp = self.handle_request(req).await?;
                Ok(Some(JSONRPCMessage::Response(resp)))
            }
            JSONRPCMessage::Notification { .. } => {
                // No response for notifications.
                Ok(None)
            }
            JSONRPCMessage::Response(_) => {
                // Server shouldn't receive responses; ignore.
                Ok(None)
            }
        }
    }

    /// Handle a single JSON-RPC request and produce a response.
    async fn handle_request(&self, req: JSONRPCRequest) -> Result<JSONRPCResponse> {
        let id = req.id.clone();
        match req.method.as_str() {
            "initialize" => Ok(JSONRPCResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {
                        "name": self.implementation.name,
                        "version": self.implementation.version,
                    },
                    "capabilities": {
                        "tools": {}
                    }
                })),
                error: None,
            }),
            "ping" => Ok(JSONRPCResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(Value::Object(serde_json::Map::new())),
                error: None,
            }),
            "tools/list" => {
                let tools = self.registry.definitions().await;
                let result = ListToolsResult { tools };
                Ok(JSONRPCResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(serde_json::to_value(&result)?),
                    error: None,
                })
            }
            "tools/call" => self.handle_tool_call(req, id).await,
            other => Ok(JSONRPCResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JSONRPCError {
                    code: ERR_METHOD_NOT_FOUND,
                    message: format!("method `{other}` not found"),
                    data: None,
                }),
            }),
        }
    }

    /// Handle a `tools/call` request.
    async fn handle_tool_call(&self, req: JSONRPCRequest, id: Value) -> Result<JSONRPCResponse> {
        let params = req.params.clone().unwrap_or(Value::Null);
        // Missing `name` is a client error, not a server-fatal condition.
        // Return a JSON-RPC -32602 (invalid params) response instead of
        // propagating `?`, which would terminate the whole connection and
        // leave the client without any response (JSON-RPC 2.0 violation).
        let tool_name = match params.get("name").and_then(Value::as_str) {
            Some(n) => n,
            None => {
                return Ok(JSONRPCResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: None,
                    error: Some(JSONRPCError {
                        code: ERR_INVALID_PARAMS,
                        message: "missing `name` in tools/call".into(),
                        data: None,
                    }),
                });
            }
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        let (def, handler) = match self.registry.get_entry(tool_name).await {
            Some(entry) => entry,
            None => {
                return Ok(JSONRPCResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: None,
                    error: Some(JSONRPCError {
                        code: ERR_METHOD_NOT_FOUND,
                        message: format!("tool `{tool_name}` not found"),
                        data: None,
                    }),
                });
            }
        };
        // Lightweight input-schema validation: every required argument must
        // be present (and non-null). Catches the common missing-argument
        // mistake before the handler runs.
        if let Err(e) = validate_arguments(&def.input_schema, &arguments) {
            return Ok(JSONRPCResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JSONRPCError {
                    code: ERR_INVALID_PARAMS,
                    message: e.to_string(),
                    data: None,
                }),
            });
        }
        match handler.call(&arguments).await {
            Ok(tcr) => {
                let result = serde_json::json!({
                    "content": tcr.content,
                    "isError": tcr.is_error,
                });
                Ok(JSONRPCResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(result),
                    error: None,
                })
            }
            Err(e) => {
                let err_text = e.to_string();
                // Wrap as a tool-call error result, not a JSON-RPC error, so
                // the client sees a structured failure.
                let result = serde_json::json!({
                    "content": [ContentBlock::text(err_text.clone())],
                    "isError": true,
                });
                Ok(JSONRPCResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(result),
                    error: None,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::ToolCallResult;
    use async_trait::async_trait;

    /// A test transport that yields pre-loaded messages and collects
    /// responses for assertions.
    struct VecTransport {
        inbox: Vec<JSONRPCMessage>,
        outbox: Vec<JSONRPCMessage>,
    }

    #[async_trait]
    impl Transport for VecTransport {
        async fn recv(&mut self) -> Result<Option<JSONRPCMessage>> {
            Ok(self.inbox.pop())
        }
        async fn send(&mut self, msg: &JSONRPCMessage) -> Result<()> {
            self.outbox.push(msg.clone());
            Ok(())
        }
    }

    /// A no-op tool handler used in tests.
    struct NoopHandler;
    #[async_trait]
    impl ToolHandler for NoopHandler {
        async fn call(&self, _args: &Value) -> Result<ToolCallResult> {
            Ok(ToolCallResult::text("noop"))
        }
    }

    /// Objective: Verify initialize returns serverInfo and capabilities.
    /// Invariants: result.serverInfo.name == impl.name; capabilities.tools present.
    #[tokio::test]
    async fn initialize_response_shape() {
        let server = MCPServer::new(Implementation {
            name: "test".into(),
            version: "1.0.0".into(),
        });
        let mut t = VecTransport {
            inbox: vec![JSONRPCMessage::Request(JSONRPCRequest {
                jsonrpc: "2.0".into(),
                id: Value::from(1),
                method: "initialize".into(),
                params: None,
            })],
            outbox: vec![],
        };
        server.serve(&mut t).await.expect("serve");
        assert_eq!(t.outbox.len(), 1, "one response sent");
        match &t.outbox[0] {
            JSONRPCMessage::Response(resp) => {
                let result = resp.result.as_ref().expect("result present");
                assert_eq!(
                    result
                        .get("serverInfo")
                        .and_then(|v| v.get("name"))
                        .and_then(Value::as_str),
                    Some("test")
                );
                assert!(result.get("capabilities").is_some());
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    /// Objective: Verify tools/list returns the registered tool.
    /// Invariants: tools array has one entry with the expected name.
    #[tokio::test]
    async fn tools_list_returns_registered() {
        let mut builder = ServerBuilder::new(Implementation {
            name: "test".into(),
            version: "1.0.0".into(),
        });
        builder = builder
            .tool(
                ToolDefinition {
                    name: "noop".into(),
                    description: "no-op tool".into(),
                    input_schema: serde_json::json!({"type": "object"}),
                },
                Arc::new(NoopHandler),
            )
            .await;
        let server = builder.build();
        let mut t = VecTransport {
            inbox: vec![JSONRPCMessage::Request(JSONRPCRequest {
                jsonrpc: "2.0".into(),
                id: Value::from(2),
                method: "tools/list".into(),
                params: None,
            })],
            outbox: vec![],
        };
        server.serve(&mut t).await.expect("serve");
        match &t.outbox[0] {
            JSONRPCMessage::Response(resp) => {
                let tools = resp
                    .result
                    .as_ref()
                    .expect("result")
                    .get("tools")
                    .and_then(Value::as_array)
                    .expect("tools array");
                assert_eq!(tools.len(), 1);
                assert_eq!(tools[0].get("name").and_then(Value::as_str), Some("noop"));
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    /// Objective: Verify unknown method returns method-not-found error.
    /// Invariants: error.code == -32601.
    #[tokio::test]
    async fn unknown_method_error() {
        let server = MCPServer::new(Implementation {
            name: "test".into(),
            version: "1.0.0".into(),
        });
        let mut t = VecTransport {
            inbox: vec![JSONRPCMessage::Request(JSONRPCRequest {
                jsonrpc: "2.0".into(),
                id: Value::from(3),
                method: "frobnicate".into(),
                params: None,
            })],
            outbox: vec![],
        };
        server.serve(&mut t).await.expect("serve");
        match &t.outbox[0] {
            JSONRPCMessage::Response(resp) => {
                let err = resp.error.as_ref().expect("error");
                assert_eq!(err.code, ERR_METHOD_NOT_FOUND);
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    /// Objective: Verify tools/call invokes the registered handler.
    /// Invariants: result.content[0].text == "noop"; isError=false.
    #[tokio::test]
    async fn tools_call_invokes_handler() {
        let mut builder = ServerBuilder::new(Implementation {
            name: "test".into(),
            version: "1.0.0".into(),
        });
        builder = builder
            .tool(
                ToolDefinition {
                    name: "noop".into(),
                    description: "no-op tool".into(),
                    input_schema: serde_json::json!({"type": "object"}),
                },
                Arc::new(NoopHandler),
            )
            .await;
        let server = builder.build();
        let mut t = VecTransport {
            inbox: vec![JSONRPCMessage::Request(JSONRPCRequest {
                jsonrpc: "2.0".into(),
                id: Value::from(4),
                method: "tools/call".into(),
                params: Some(serde_json::json!({
                    "name": "noop",
                    "arguments": {}
                })),
            })],
            outbox: vec![],
        };
        server.serve(&mut t).await.expect("serve");
        match &t.outbox[0] {
            JSONRPCMessage::Response(resp) => {
                let result = resp.result.as_ref().expect("result");
                let content = result
                    .get("content")
                    .and_then(Value::as_array)
                    .expect("content array");
                assert_eq!(content.len(), 1);
                assert_eq!(content[0].get("text").and_then(Value::as_str), Some("noop"));
                assert_eq!(result.get("isError").and_then(Value::as_bool), Some(false));
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    /// Objective: Verify tools/call on unknown tool returns method-not-found.
    /// Invariants: error.code == -32601, message contains tool name.
    #[tokio::test]
    async fn tools_call_unknown_tool() {
        let server = MCPServer::new(Implementation {
            name: "test".into(),
            version: "1.0.0".into(),
        });
        let mut t = VecTransport {
            inbox: vec![JSONRPCMessage::Request(JSONRPCRequest {
                jsonrpc: "2.0".into(),
                id: Value::from(5),
                method: "tools/call".into(),
                params: Some(serde_json::json!({
                    "name": "ghost",
                    "arguments": {}
                })),
            })],
            outbox: vec![],
        };
        server.serve(&mut t).await.expect("serve");
        match &t.outbox[0] {
            JSONRPCMessage::Response(resp) => {
                let err = resp.error.as_ref().expect("error");
                assert_eq!(err.code, ERR_METHOD_NOT_FOUND);
                assert!(err.message.contains("ghost"));
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    /// Objective: Verify tools/call rejects missing required arguments.
    /// Invariants: error.code == -32602 (invalid params) and names the arg.
    #[tokio::test]
    async fn tools_call_validates_required_args() {
        let mut builder = ServerBuilder::new(Implementation {
            name: "test".into(),
            version: "1.0.0".into(),
        });
        builder = builder
            .tool(
                ToolDefinition {
                    name: "needs_foo".into(),
                    description: "requires foo".into(),
                    input_schema: serde_json::json!({"type": "object", "required": ["foo"]}),
                },
                Arc::new(NoopHandler),
            )
            .await;
        let server = builder.build();
        let mut t = VecTransport {
            inbox: vec![JSONRPCMessage::Request(JSONRPCRequest {
                jsonrpc: "2.0".into(),
                id: Value::from(6),
                method: "tools/call".into(),
                params: Some(serde_json::json!({
                    "name": "needs_foo",
                    "arguments": {}
                })),
            })],
            outbox: vec![],
        };
        server.serve(&mut t).await.expect("serve");
        match &t.outbox[0] {
            JSONRPCMessage::Response(resp) => {
                let err = resp.error.as_ref().expect("error present");
                assert_eq!(err.code, ERR_INVALID_PARAMS);
                assert!(err.message.contains("foo"));
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    /// Objective: Verify notifications produce no response.
    /// Invariants: outbox is empty after a notification.
    #[tokio::test]
    async fn notification_no_response() {
        let server = MCPServer::new(Implementation {
            name: "test".into(),
            version: "1.0.0".into(),
        });
        let mut t = VecTransport {
            inbox: vec![JSONRPCMessage::Notification {
                jsonrpc: "2.0".into(),
                method: "notifications/initialized".into(),
                params: None,
            }],
            outbox: vec![],
        };
        server.serve(&mut t).await.expect("serve");
        assert!(t.outbox.is_empty(), "no response to notification");
    }
}
