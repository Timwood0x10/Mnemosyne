//! `memory_compile` MCP tool backed by the unified cognition compiler.
//!
//! The tool persists only immutable `Fact` records. Legacy knowledge,
//! decisions, session state, and prompts remain compatibility projections in
//! the response and are never treated as a second cognition source of truth.

use std::sync::Arc;

use serde_json::Value;

use crate::cognition::FactStore;
use crate::cognition_compiler::CognitionCompiler;
use crate::distiller::{Distiller, PipelineDistiller};
use crate::error::Error;
use crate::fact_store::SqliteFactStore;
use crate::mcp::types::{ToolCallResult, ToolDefinition, ToolHandler};
use crate::prompt::PromptBuilder;
use crate::types::{Experience, Memory, MemoryType, Message};

/// Handler for the unified conversation-to-cognition compilation path.
pub struct MemoryCompileTool {
    distiller: Option<Arc<PipelineDistiller>>,
    fact_store: Arc<SqliteFactStore>,
    compiler: CognitionCompiler,
}

impl MemoryCompileTool {
    /// Create the tool with the shared production stores.
    pub fn new(
        distiller: Option<Arc<PipelineDistiller>>,
        fact_store: Arc<SqliteFactStore>,
    ) -> Self {
        Self {
            distiller,
            fact_store,
            compiler: CognitionCompiler::new(),
        }
    }
}

/// Return the stable MCP schema for `memory_compile`.
pub fn memory_compile_definition() -> ToolDefinition {
    ToolDefinition {
        name: "memory_compile".into(),
        description: "Compile conversation into structured knowledge + decisions + session state. Optionally distill memories.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "messages": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "role": {"type": "string", "enum": ["user", "assistant", "system", "tool"]},
                            "content": {"type": "string"},
                            "turn_id": {"type": "string"},
                            "tool_call_id": {"type": "string"}
                        },
                        "required": ["role", "content"]
                    }
                },
                "distill": {"type": "boolean", "default": false, "description": "Also run distillation pipeline"},
                "conversation_id": {"type": "string", "description": "Required when distill=true"},
                "tenant_id": {"type": "string", "default": "default"},
                "user_id": {"type": "string"}
            },
            "required": ["messages"]
        }),
    }
}

#[async_trait::async_trait]
impl ToolHandler for MemoryCompileTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let messages_raw = args
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::InvalidInput("missing `messages` array".into()))?;
        let messages = parse_messages(messages_raw)?;
        let tenant_id = args
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let user_id = args.get("user_id").and_then(Value::as_str).unwrap_or("");

        let user_entity_id = self.fact_store.resolve_user(tenant_id, user_id)?;
        let logical_time = chrono::Utc::now().timestamp() as i32;
        let compiled =
            self.compiler
                .compile_conversation(tenant_id, &messages, user_entity_id, logical_time);
        let stored_facts = self.fact_store.insert_batch(&compiled.facts)?;

        let builder = PromptBuilder;
        let recent_count = messages.len().min(6);
        let prompt = builder.build(
            &messages[messages.len() - recent_count..],
            &compiled.compatibility,
        );
        let memories = self
            .distill_if_requested(args, tenant_id, user_id, &messages, &compiled.compatibility)
            .await?;

        let payload = serde_json::json!({
            "knowledge": compiled.compatibility.knowledge,
            "decisions": compiled.compatibility.decisions,
            "session": compiled.compatibility.session,
            "prompt": prompt,
            "cognition": {
                "user_entity_id": user_entity_id,
                "tenant_id": tenant_id,
                "user_id": if user_id.is_empty() { "default" } else { user_id },
                "observations_compiled": compiled.observations.len(),
                "facts_compiled": compiled.facts.len(),
                "facts_stored": stored_facts,
            },
            "distilled_memories": memories,
        });
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

impl MemoryCompileTool {
    async fn distill_if_requested(
        &self,
        args: &Value,
        tenant_id: &str,
        user_id: &str,
        messages: &[Message],
        compatibility: &crate::types::CompiledConversation,
    ) -> Result<Option<Vec<Memory>>, Error> {
        let should_distill = args
            .get("distill")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !should_distill {
            return Ok(None);
        }
        let Some(distiller) = &self.distiller else {
            return Ok(None);
        };

        let conversation_id = args
            .get("conversation_id")
            .and_then(Value::as_str)
            .unwrap_or("compile");
        let memories = distiller
            .distill(conversation_id, messages, tenant_id, user_id)
            .await?;
        persist_compatible_decisions(distiller, tenant_id, &compatibility.decisions).await?;
        Ok(Some(memories))
    }
}

async fn persist_compatible_decisions(
    distiller: &PipelineDistiller,
    tenant_id: &str,
    decisions: &[crate::types::Decision],
) -> Result<(), Error> {
    let noise_filter = crate::filter::NoiseFilter::new();
    let security_filter = crate::filter::SecurityFilter::new();
    let existing = distiller
        .store()
        .get_by_memory_type(tenant_id, MemoryType::Knowledge)
        .await?;

    for decision in decisions {
        let content = format!(
            "Decision: {} — Rationale: {}",
            decision.decision, decision.rationale
        );
        let probe = Message::new("user", &content);
        if security_filter.is_sensitive(&probe)
            || noise_filter.is_noise(&probe)
            || existing
                .iter()
                .any(|experience| experience.content == content)
        {
            continue;
        }
        let mut experience = Experience::new(
            tenant_id,
            MemoryType::Knowledge,
            content,
            decision.importance,
        );
        experience.source = "compile".to_string();
        distiller.store().create(&experience).await?;
    }
    Ok(())
}

fn parse_messages(values: &[Value]) -> Result<Vec<Message>, Error> {
    values
        .iter()
        .map(|raw| {
            let object = raw
                .as_object()
                .ok_or_else(|| Error::InvalidInput("each message must be an object".into()))?;
            let role = object
                .get("role")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::InvalidInput("message missing `role`".into()))?;
            let content = object
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::InvalidInput("message missing `content`".into()))?;
            let mut message = Message::new(role, content);
            if let Some(turn_id) = object.get("turn_id").and_then(Value::as_str) {
                message.turn_id = Some(turn_id.to_string());
            }
            if let Some(tool_call_id) = object.get("tool_call_id").and_then(Value::as_str) {
                message.tool_call_id = Some(tool_call_id.to_string());
            }
            Ok(message)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result_payload(result: &ToolCallResult) -> Value {
        let text = result
            .content
            .first()
            .and_then(|block| block.text.as_deref())
            .expect("A successful compile result must contain one text payload");
        serde_json::from_str(text).expect("The compile text payload must be valid JSON")
    }

    /// Objective: Verify the MCP compatibility contract after moving the handler.
    /// Invariants: Legacy fields remain and cognition facts use the resolved user identity.
    #[tokio::test]
    async fn handler_preserves_legacy_fields_and_persists_cognition() {
        let fact_store = Arc::new(
            SqliteFactStore::open_in_memory()
                .expect("An isolated fact store must initialize for the handler test"),
        );
        let tool = MemoryCompileTool::new(None, fact_store.clone());
        let result = tool
            .call(&serde_json::json!({
                "tenant_id": "tenant-a",
                "user_id": "alice",
                "messages": [
                    {"role": "user", "content": "I want to learn Rust."},
                    {"role": "assistant", "content": "Let us start."}
                ]
            }))
            .await
            .expect("The unified memory_compile handler should accept valid messages");
        let payload = result_payload(&result);

        for field in [
            "knowledge",
            "decisions",
            "session",
            "prompt",
            "cognition",
            "distilled_memories",
        ] {
            assert!(
                payload.get(field).is_some(),
                "The compatibility response must retain the `{field}` field"
            );
        }
        let entity_id = payload["cognition"]["user_entity_id"]
            .as_i64()
            .expect("The cognition response must expose a numeric user entity id");
        let facts = fact_store
            .get_facts(entity_id)
            .expect("Facts emitted by memory_compile must remain readable");
        assert_eq!(
            payload["cognition"]["facts_stored"].as_u64(),
            Some(facts.len() as u64),
            "The reported stored count must match the committed fact rows"
        );
        assert!(
            !facts.is_empty(),
            "A supported user goal marker must persist at least one immutable fact"
        );
    }

    /// Objective: Verify optional message identifiers survive MCP parsing.
    /// Invariants: Turn and tool-call identifiers remain attached to parsed messages.
    #[test]
    fn parser_preserves_optional_message_identifiers() {
        let messages = parse_messages(&[serde_json::json!({
            "role": "tool",
            "content": "completed",
            "turn_id": "turn-7",
            "tool_call_id": "call-9"
        })])
        .expect("A tool result with optional identifiers should parse");

        assert_eq!(
            messages[0].turn_id.as_deref(),
            Some("turn-7"),
            "The parser must preserve the logical turn identifier"
        );
        assert_eq!(
            messages[0].tool_call_id.as_deref(),
            Some("call-9"),
            "The parser must preserve the tool call identifier"
        );
    }

    /// Objective: Verify malformed message objects fail with typed input errors.
    /// Invariants: Invalid requests do not create a default user or persist facts.
    #[tokio::test]
    async fn handler_rejects_message_without_content() {
        let fact_store = Arc::new(
            SqliteFactStore::open_in_memory()
                .expect("An isolated fact store must initialize for invalid-input testing"),
        );
        let tool = MemoryCompileTool::new(None, fact_store.clone());
        let error = tool
            .call(&serde_json::json!({"messages": [{"role": "user"}]}))
            .await
            .expect_err("A message without content must fail before identity resolution");

        assert!(
            matches!(error, Error::InvalidInput(ref message) if message.contains("content")),
            "The missing content error must remain a typed InvalidInput, got {error:?}"
        );
        assert!(
            fact_store
                .find_entity("default", Some("default"), "User")
                .expect("Entity lookup after invalid input must succeed")
                .is_none(),
            "Invalid input must not create the default user as a side effect"
        );
    }
}
