//! MCP wire-protocol types.
//!
//! These types mirror the JSON-RPC 2.0 messages used by the Model Context
//! Protocol. They are deliberately permissive on deserialization (extra
//! fields are ignored) and tight on serialization (only documented fields
//! are emitted).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

/// The tenant / user a tool call runs as when the caller omits it.
///
/// This engine is a single-node MCP server: `tenant_id` is the local
/// deployment and `user_id` is the person using it, so an omitted value means
/// "the one local tenant / user", never "anonymous". Tools used to pass a raw
/// `""` down to the store while echoing `"default"` back to the caller, so one
/// caller's rows landed under two identities — and, because conflict resolution
/// only compares rows with an equal `user_id`, they never deduplicated against
/// each other.
pub const DEFAULT_IDENTITY: &str = "default";

/// The `field` argument, trimmed, or [`DEFAULT_IDENTITY`] when absent or blank.
///
/// Every tool reads its identities through this helper so the value written to
/// the store, the value echoed in the response and the value used for entity
/// resolution cannot drift apart.
#[must_use]
pub fn identity_arg<'a>(args: &'a Value, field: &str) -> &'a str {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_IDENTITY)
}

/// Server implementation identification, sent in `initialize` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Implementation {
    /// Human-readable server name.
    pub name: String,
    /// Semver version string.
    pub version: String,
}

/// A single content block in a tool-call result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentBlock {
    /// Block type, currently always `"text"`.
    #[serde(rename = "type")]
    pub block_type: String,
    /// Text payload (when `block_type == "text"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// MIME type hint (optional).
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "mimeType")]
    pub mime_type: Option<String>,
}

impl ContentBlock {
    /// Build a text content block.
    #[must_use]
    pub fn text(payload: impl Into<String>) -> Self {
        Self {
            block_type: "text".to_string(),
            text: Some(payload.into()),
            mime_type: None,
        }
    }
}

/// Result returned by a tool handler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResult {
    /// Content blocks to return to the client.
    pub content: Vec<ContentBlock>,
    /// If `true`, the tool call failed gracefully (the error is in `content`).
    ///
    /// Serialized as `isError` to match the MCP wire contract and what the
    /// handlers emit (NEW-M11). Previously the field name `is_error` leaked
    /// into the JSON while handlers wrote `isError` — two spellings for one
    /// flag.
    #[serde(default, skip_serializing_if = "is_false", rename = "isError")]
    pub is_error: bool,
}

impl ToolCallResult {
    /// Build a successful result with a single text block.
    #[must_use]
    pub fn text(payload: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::text(payload)],
            is_error: false,
        }
    }

    /// Build an error result with a single text block.
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::text(message)],
            is_error: true,
        }
    }
}

fn is_false(b: &bool) -> bool {
    !b
}

/// Static definition of a tool exposed by the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool name (unique within a server).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema describing the tool's `arguments` object.
    ///
    /// Serialized as `inputSchema` per the MCP wire contract (camelCase) —
    /// previously the snake_case `input_schema` leaked into `tools/list`,
    /// which strict MCP clients reject (matches codescope's
    /// `#[serde(rename = "inputSchema")]`).
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// Async handler invoked when a client calls a tool.
///
/// Implementations parse `args`, perform the work, and return a
/// [`ToolCallResult`].
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Invoke the tool.
    ///
    /// # Arguments
    ///
    /// * `args` - The JSON object passed by the client as `params.arguments`.
    async fn call(&self, args: &Value) -> Result<ToolCallResult>;
}

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JSONRPCRequest {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Client-provided id (number or string).
    pub id: Value,
    /// Method name.
    pub method: String,
    /// Method parameters (object or absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 response envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JSONRPCResponse {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Echo of the client id.
    pub id: Value,
    /// Result payload (mutually exclusive with `error`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error payload (mutually exclusive with `result`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JSONRPCError>,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JSONRPCError {
    /// Numeric error code.
    pub code: i64,
    /// Short error message.
    pub message: String,
    /// Optional additional data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Top-level message: request, response, or notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JSONRPCMessage {
    /// A request expecting a response.
    Request(JSONRPCRequest),
    /// A response to a prior request.
    Response(JSONRPCResponse),
    /// A notification: no `id`, no response expected.
    Notification {
        /// Always `"2.0"`.
        jsonrpc: String,
        /// Notification method name.
        method: String,
        /// Notification parameters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<Value>,
    },
}

/// Result payload for `tools/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListToolsResult {
    /// All tools registered on the server.
    pub tools: Vec<ToolDefinition>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify ContentBlock::text produces the expected shape.
    /// Invariants: type=="text", text set.
    #[test]
    fn content_block_text_shape() {
        let b = ContentBlock::text("hi");
        assert_eq!(b.block_type, "text", "a text block must serialise its type");
        assert_eq!(
            b.text.as_deref(),
            Some("hi"),
            "a text block must carry its payload"
        );
        assert!(b.mime_type.is_none(), "a text block carries no mime type");
    }

    /// Objective: Verify ToolCallResult::error sets is_error=true.
    /// Invariants: error() returns is_error=true with text content.
    #[test]
    fn tool_call_result_error() {
        let r = ToolCallResult::error("bad input");
        assert!(r.is_error, "a failure result must set is_error");
        assert_eq!(
            r.content.len(),
            1,
            "a failure result must carry exactly one content block"
        );
        assert_eq!(
            r.content[0].text.as_deref(),
            Some("bad input"),
            "the failure result must echo the message"
        );
    }

    /// Objective: Verify `ToolDefinition` serializes its schema as
    /// `inputSchema` (MCP camelCase wire contract), not snake_case — the
    /// regression fixed against codescope's protocol.rs.
    /// Invariants: JSON has `inputSchema` key; no `input_schema` key.
    #[test]
    fn tool_definition_serializes_input_schema_camel_case() {
        let def = ToolDefinition {
            name: "demo".into(),
            description: "demo tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let json = serde_json::to_value(&def).expect("serialize");
        assert!(
            json.get("inputSchema").is_some(),
            "has inputSchema, got {json}"
        );
        assert!(
            json.get("input_schema").is_none(),
            "no snake_case, got {json}"
        );
    }

    /// Objective: Verify JSONRPCMessage can deserialize a Request.
    /// Invariants: Request variant with method == "initialize".
    #[test]
    fn message_deserializes_request() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let msg: JSONRPCMessage = serde_json::from_str(json).expect("parse");
        match msg {
            JSONRPCMessage::Request(r) => {
                assert_eq!(
                    r.method, "initialize",
                    "the request method must be preserved"
                );
                assert_eq!(r.id, Value::from(1), "the request id must be preserved");
            }
            other => panic!("expected Request, got {other:?}"),
        }
    }

    /// Objective: Verify JSONRPCMessage can deserialize a Notification.
    /// Invariants: Notification variant carries the method name.
    #[test]
    fn message_deserializes_notification() {
        let json = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let msg: JSONRPCMessage = serde_json::from_str(json).expect("parse");
        match msg {
            JSONRPCMessage::Notification { method, .. } => {
                assert_eq!(
                    method, "notifications/initialized",
                    "the notification method must be preserved"
                );
            }
            other => panic!("expected Notification, got {other:?}"),
        }
    }

    /// Objective: Verify an omitted or blank identity resolves to the documented
    /// default (and a padded one is trimmed), so every tool writes, resolves and
    /// echoes the same value. Tools used to hand a raw `""` to the store while
    /// reporting `"default"`, splitting one caller across two identities that
    /// conflict resolution never compared.
    /// Invariants: absent/blank/non-string → `default`; surrounding whitespace
    /// is dropped; a real id is returned as given.
    #[test]
    fn identity_arg_defaults_blank_values_and_trims() {
        let absent = serde_json::json!({});
        assert_eq!(identity_arg(&absent, "user_id"), DEFAULT_IDENTITY);

        let blank = serde_json::json!({ "user_id": "   " });
        assert_eq!(
            identity_arg(&blank, "user_id"),
            DEFAULT_IDENTITY,
            "a blank id is an omitted id"
        );

        let padded = serde_json::json!({ "user_id": "  alice " });
        assert_eq!(identity_arg(&padded, "user_id"), "alice");

        let wrong_type = serde_json::json!({ "user_id": 7 });
        assert_eq!(
            identity_arg(&wrong_type, "user_id"),
            DEFAULT_IDENTITY,
            "a non-string id must not be coerced into an identity"
        );
    }
}
