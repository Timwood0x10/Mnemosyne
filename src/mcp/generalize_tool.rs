//! MCP tool for the generalization pipeline.
//!
//! Registers `generalize_compile`, the production entry point that compiles
//! ANY caller-provided data source — a pasted conversation (`dialog`) or raw
//! prose (`text`) — into the unified knowledge graph. It is the wiring that
//! connects the three generalization modules to a live MCP tool:
//!
//! | Module                       | Role                                     |
//! |------------------------------|------------------------------------------|
//! | `knowledge/document_source`  | unify any data into an `ExternalDoc`     |
//! | `knowledge/domain_profile`   | pluggable per-domain extraction rules    |
//! | `compiler/pipeline`          | load → split → extract → persist         |
//!
//! Without this tool those modules are only exercised by tests; this handler
//! makes them reachable from a real agent so external data can be ingested
//! and later retrieved to sustain the AI persona.

use std::sync::Arc;

use serde_json::Value;

use crate::compiler::pipeline::compile_source;
use crate::error::Error;
use crate::knowledge::SQLiteKnowledgeStore;
use crate::knowledge::document_source::{DialogSource, DocumentSource, RawTextSource};
use crate::knowledge::domain_profile::{DomainProfile, conversation_profile};
use crate::mcp::server::ServerBuilder;
use crate::mcp::types::{ContentBlock, ToolCallResult, ToolDefinition, ToolHandler};
use crate::types::Message;

/// Extract an optional `&str` argument from the JSON args object.
fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// Build a success [`ToolCallResult`] containing a single JSON text block.
fn json_ok(value: &impl serde::Serialize) -> Result<ToolCallResult, Error> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| Error::Internal(format!("serialize result: {e}")))?;
    Ok(ToolCallResult {
        content: vec![ContentBlock {
            block_type: "text".into(),
            text: Some(text),
            mime_type: Some("application/json".into()),
        }],
        is_error: false,
    })
}

/// Build a graceful error [`ToolCallResult`] (the tool call "succeeded" as a
/// protocol exchange but reports a user-facing failure in its content).
fn err_result(message: impl Into<String>) -> ToolCallResult {
    ToolCallResult {
        content: vec![ContentBlock::text(message.into())],
        is_error: true,
    }
}

/// Parse the `messages` array from a tool call into [`Message`] values.
fn parse_messages(arr: &[Value]) -> Result<Vec<Message>, Error> {
    let mut out = Vec::with_capacity(arr.len());
    for raw in arr {
        let obj = raw
            .as_object()
            .ok_or_else(|| Error::InvalidInput("each message must be an object".into()))?;
        let role = obj
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("message missing `role`".into()))?;
        let content = obj
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("message missing `content`".into()))?;
        out.push(Message::new(role, content));
    }
    Ok(out)
}

/// Default document title used when the caller does not supply one.
fn default_title() -> String {
    let ts = chrono::Utc::now().timestamp();
    format!("generalize-{ts}")
}

/// The `generalize_compile` tool handler.
pub struct GeneralizeCompileHandler {
    store: Arc<SQLiteKnowledgeStore>,
}

#[async_trait::async_trait]
impl ToolHandler for GeneralizeCompileHandler {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        // `doc_type` selects the source adapter. Defaults to `text` (raw
        // prose) so a bare `{"text": ...}` call "just works".
        let doc_type = opt_str(args, "doc_type").unwrap_or("text");
        let title = opt_str(args, "title")
            .map(ToOwned::to_owned)
            .unwrap_or_else(default_title);
        let source = opt_str(args, "source").unwrap_or("generalize_compile");
        let tenant = opt_str(args, "tenant_id").unwrap_or("default");

        // Resolve the domain profile: an explicit name loads on demand;
        // otherwise fall back to the process-cached conversation pack. A
        // missing/unparseable pack is a user-facing error, reported gracefully.
        let profile: DomainProfile = match opt_str(args, "profile") {
            Some(name) => match DomainProfile::load(name) {
                Ok(p) => p,
                Err(e) => return Ok(err_result(format!("load profile `{name}`: {e}"))),
            },
            None => conversation_profile().clone(),
        };

        // Build the unified document source from the caller's payload. Input
        // validation failures are user-facing errors (graceful result), not
        // protocol-level failures.
        let boxed: Box<dyn DocumentSource> = match doc_type {
            "dialog" => {
                let Some(raw) = args.get("messages").and_then(Value::as_array) else {
                    return Ok(err_result("doc_type=dialog requires `messages` array"));
                };
                let messages = match parse_messages(raw) {
                    Ok(m) => m,
                    Err(e) => return Ok(err_result(e.to_string())),
                };
                if messages.is_empty() {
                    return json_ok(&serde_json::json!({
                        "compiled": true,
                        "doc_type": "dialog",
                        "title": title,
                        "message": "no messages supplied; nothing compiled",
                        "stats": {"documents": 0, "objects": 0, "edges": 0, "evidence": 0},
                    }));
                }
                Box::new(DialogSource::new(title.clone(), source, messages))
            }
            "text" => {
                let Some(text) = args.get("text").and_then(Value::as_str) else {
                    return Ok(err_result("doc_type=text requires `text` string"));
                };
                Box::new(RawTextSource::new(title.clone(), source, text, "text"))
            }
            other => {
                return Ok(err_result(format!(
                    "unsupported doc_type `{other}`; expected `dialog` or `text`"
                )));
            }
        };

        // Persist through the unified pipeline. Persistence failures are
        // surfaced as a graceful error result so the client sees the message.
        match compile_source(boxed.as_ref(), &profile, self.store.as_ref(), tenant).await {
            Ok(stats) => json_ok(&serde_json::json!({
                "compiled": true,
                "doc_type": doc_type,
                "title": title,
                "profile": profile.profile_name,
                "stats": {
                    "documents": stats.documents,
                    "objects": stats.objects,
                    "edges": stats.edges,
                    "evidence": stats.evidence,
                },
            })),
            Err(e) => Ok(err_result(format!("compile failed: {e}"))),
        }
    }
}

/// Register the `generalize_compile` tool on `builder`.
pub async fn register_generalize_tool(
    builder: ServerBuilder,
    store: Arc<SQLiteKnowledgeStore>,
) -> ServerBuilder {
    builder
        .tool(
            ToolDefinition {
                name: "generalize_compile".into(),
                description: "Compile ANY external data source (pasted conversation or raw prose) into the unified knowledge graph as documents, entities, edges and evidence. Use doc_type=dialog with a `messages` array for conversations, or doc_type=text with a `text` string for arbitrary prose. The compiled knowledge is later retrievable to sustain the AI persona.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "doc_type": {"type": "string", "enum": ["dialog", "text"], "default": "text", "description": "dialog = conversation messages; text = arbitrary prose"},
                        "messages": {
                            "type": "array",
                            "description": "Conversation messages (required when doc_type=dialog)",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "role": {"type": "string", "enum": ["user", "assistant", "system"]},
                                    "content": {"type": "string"}
                                },
                                "required": ["role", "content"]
                            }
                        },
                        "text": {"type": "string", "description": "Raw prose / notes (required when doc_type=text)"},
                        "title": {"type": "string", "description": "Optional document title; defaults to a generated id"},
                        "source": {"type": "string", "description": "Optional provenance origin tag"},
                        "profile": {"type": "string", "default": "conversation_cognition", "description": "Optional domain profile pack name from config/domain_profiles/"},
                        "tenant_id": {"type": "string", "default": "default"}
                    },
                    "oneOf": [
                        {"required": ["messages"]},
                        {"required": ["text"]}
                    ]
                }),
            },
            Arc::new(GeneralizeCompileHandler { store }),
        )
        .await
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an in-memory store ready for compilation.
    async fn memory_store() -> Arc<SQLiteKnowledgeStore> {
        Arc::new(
            SQLiteKnowledgeStore::open_in_memory()
                .await
                .expect("open in-memory store"),
        )
    }

    /// Objective: Verify a dialog compiles into the graph with the
    /// conversation-cognition profile WITHOUT creating a `person` object named
    /// after the session title (a session id is not a person — the fixed
    /// modeling; speakers live in the cognition layer instead).
    /// Invariants: one document; objects == 0; result is not an error.
    #[tokio::test]
    async fn dialog_compiles_to_entities() {
        let handler = GeneralizeCompileHandler {
            store: memory_store().await,
        };
        let result = handler
            .call(&serde_json::json!({
                "doc_type": "dialog",
                "messages": [
                    {"role": "user", "content": "我喜欢 Rust，目标是做稳定的编译器。"},
                    {"role": "assistant", "content": "好的，我们一步步来。"}
                ],
                "title": "会话-通用测试"
            }))
            .await
            .expect("handler returns");
        assert!(!result.is_error, "dialog compile must succeed");
        let text = result.content[0].text.clone().unwrap_or_default();
        assert!(
            text.contains("\"compiled\": true"),
            "result marks compiled, got: {text}"
        );
        assert!(
            text.contains("\"objects\": 0"),
            "dialog title must not become an object, got: {text}"
        );
        assert!(
            text.contains("\"documents\": 1"),
            "one dialog document compiled, got: {text}"
        );
    }

    /// Objective: Verify raw text (no messages) compiles into the graph using
    /// the default `text` doc_type.
    /// Invariants: one document; >= 1 object; doc_type echoed as "text".
    #[tokio::test]
    async fn raw_text_compiles_with_default_doc_type() {
        let handler = GeneralizeCompileHandler {
            store: memory_store().await,
        };
        let result = handler
            .call(&serde_json::json!({
                "text": "我偏好简洁的架构，反对过度抽象。"
            }))
            .await
            .expect("handler returns");
        assert!(!result.is_error, "raw text compile must succeed");
        let text = result.content[0].text.clone().unwrap_or_default();
        assert!(
            text.contains("\"doc_type\": \"text\""),
            "default doc_type is text"
        );
    }

    /// Objective: Verify an explicit non-default profile name is honored.
    /// Invariants: a dialog compile echoes the requested profile.
    #[tokio::test]
    async fn explicit_profile_is_honored() {
        let handler = GeneralizeCompileHandler {
            store: memory_store().await,
        };
        let result = handler
            .call(&serde_json::json!({
                "doc_type": "dialog",
                "profile": "conversation_cognition",
                "messages": [
                    {"role": "user", "content": "我计划下周二上线。"}
                ]
            }))
            .await
            .expect("handler returns");
        assert!(!result.is_error, "explicit profile must be accepted");
        let text = result.content[0].text.clone().unwrap_or_default();
        assert!(
            text.contains("conversation_cognition"),
            "profile echoed in result, got: {text}"
        );
    }

    /// Objective: Verify an unknown profile name yields a typed error, not a
    /// panic.
    /// Invariants: unsupported profile → error result with a message.
    #[tokio::test]
    async fn unknown_profile_errors_gracefully() {
        let handler = GeneralizeCompileHandler {
            store: memory_store().await,
        };
        let result = handler
            .call(&serde_json::json!({
                "doc_type": "text",
                "profile": "does_not_exist_pack",
                "text": "hello"
            }))
            .await
            .expect("handler returns");
        assert!(result.is_error, "unknown profile → error result");
    }

    /// Objective: Verify an unsupported doc_type is rejected with a clear
    /// message rather than panicking.
    /// Invariants: unsupported doc_type → error result.
    #[tokio::test]
    async fn unsupported_doc_type_errors() {
        let handler = GeneralizeCompileHandler {
            store: memory_store().await,
        };
        let result = handler
            .call(&serde_json::json!({"doc_type": "video", "text": "x"}))
            .await
            .expect("handler returns");
        assert!(result.is_error, "unsupported doc_type → error result");
        let text = result.content[0].text.clone().unwrap_or_default();
        assert!(
            text.contains("unsupported doc_type"),
            "clear message, got: {text}"
        );
    }

    /// Objective: Verify `doc_type=dialog` without a `messages` array yields a
    /// typed error.
    /// Invariants: missing messages → error result mentioning `messages`.
    #[tokio::test]
    async fn dialog_requires_messages() {
        let handler = GeneralizeCompileHandler {
            store: memory_store().await,
        };
        let result = handler
            .call(&serde_json::json!({"doc_type": "dialog"}))
            .await
            .expect("handler returns");
        assert!(result.is_error, "dialog without messages → error result");
        let text = result.content[0].text.clone().unwrap_or_default();
        assert!(
            text.contains("messages"),
            "error mentions messages, got: {text}"
        );
    }

    /// Objective: Verify an empty `messages` array is a graceful no-op, not an
    /// error or a crash.
    /// Invariants: empty messages → success with zero stats.
    #[tokio::test]
    async fn empty_dialog_is_graceful() {
        let handler = GeneralizeCompileHandler {
            store: memory_store().await,
        };
        let result = handler
            .call(&serde_json::json!({
                "doc_type": "dialog",
                "messages": []
            }))
            .await
            .expect("handler returns");
        assert!(!result.is_error, "empty dialog is graceful, not an error");
        let text = result.content[0].text.clone().unwrap_or_default();
        assert!(
            text.contains("\"objects\": 0"),
            "zero objects for empty dialog"
        );
    }

    /// Objective: Verify `doc_type=text` without a `text` string yields a
    /// graceful error (consistent with the dialog branch), not a panic.
    /// Invariants: missing text → error result mentioning `text`.
    #[tokio::test]
    async fn text_requires_text_string() {
        let handler = GeneralizeCompileHandler {
            store: memory_store().await,
        };
        let result = handler
            .call(&serde_json::json!({"doc_type": "text"}))
            .await
            .expect("handler returns");
        assert!(result.is_error, "text without text string → error result");
        let text = result.content[0].text.clone().unwrap_or_default();
        assert!(text.contains("text"), "error mentions text, got: {text}");
    }
}
