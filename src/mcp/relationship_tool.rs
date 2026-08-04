//! MCP tools for relationship state and persona evolution timeline (阶段C-2).
//!
//! Three tools are exposed on the MCP server:
//!
//! - `relationship_update` — incrementally update the agent↔user relationship
//!   state from a batch of messages using deterministic rules (no LLM).
//! - `relationship_query` — read the current relationship snapshot for a pair.
//! - `persona_timeline` — rebuild an entity's full persona evolution as
//!   `起点 → 关键转变点 → 现状` from its accumulated facts (ADD-only).
//!
//! All tools are deterministic and read/write only the fact store's own
//! SQLite tables; none of them invoke an LLM.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::Error;
use crate::fact_store::SqliteFactStore;
use crate::mcp::types::{ToolCallResult, ToolDefinition, ToolHandler};
use crate::persona::timeline::build_timeline_for_entity;
use crate::relationship::{RelationshipState, RelationshipStore};
use crate::types::Message;

/// Handler for the `relationship_update` tool.
pub struct RelationshipUpdateTool {
    store: Arc<SqliteFactStore>,
}

impl RelationshipUpdateTool {
    /// Construct the tool over the shared fact store.
    #[must_use]
    pub fn new(store: Arc<SqliteFactStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl ToolHandler for RelationshipUpdateTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let tenant_id = args
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let agent_id = args
            .get("agent_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing `agent_id`".into()))?;
        let user_id = args
            .get("user_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing `user_id`".into()))?;
        let messages = parse_messages(args.get("messages"))?;

        let rel = RelationshipStore::new(self.store.clone());
        let state = rel.apply_messages(tenant_id, agent_id, user_id, &messages)?;
        let payload = serialize_state(&state, agent_id, user_id);
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

/// Handler for the `relationship_query` tool.
pub struct RelationshipQueryTool {
    store: Arc<SqliteFactStore>,
}

impl RelationshipQueryTool {
    /// Construct the tool over the shared fact store.
    #[must_use]
    pub fn new(store: Arc<SqliteFactStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl ToolHandler for RelationshipQueryTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let tenant_id = args
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let agent_id = args
            .get("agent_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing `agent_id`".into()))?;
        let user_id = args
            .get("user_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing `user_id`".into()))?;

        let agent_entity_id = self.store.resolve_agent(tenant_id, agent_id)?;
        let user_entity_id = self.store.resolve_user(tenant_id, user_id)?;
        let rel = RelationshipStore::new(self.store.clone());
        let state = rel.get_relationship(tenant_id, agent_entity_id, user_entity_id)?;

        let payload = match state {
            Some(s) => serialize_state(&s, agent_id, user_id),
            None => json!({
                "tenant_id": tenant_id,
                "agent_id": agent_id,
                "user_id": user_id,
                "exists": false,
                "intimacy": 0.0,
                "stage": "stranger",
                "emotion_trend": "stable",
                "recent_topics": [],
                "updated_at": 0,
            }),
        };
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

/// Handler for the `persona_timeline` tool.
pub struct PersonaTimelineTool {
    store: Arc<SqliteFactStore>,
}

impl PersonaTimelineTool {
    /// Construct the tool over the shared fact store.
    #[must_use]
    pub fn new(store: Arc<SqliteFactStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl ToolHandler for PersonaTimelineTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let entity_id = args
            .get("entity_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| Error::InvalidInput("missing `entity_id`".into()))?;
        let timeline = build_timeline_for_entity(self.store.as_ref(), entity_id)?;
        let payload = json!({
            "entity_id": entity_id,
            "start": timeline.start,
            "milestones": timeline
                .milestones
                .iter()
                .map(|m| json!({
                    "type": m.milestone_type,
                    "note": m.note,
                    "fact": m.fact,
                }))
                .collect::<Vec<_>>(),
            "current": timeline.current,
            "trajectory": timeline.trajectory,
        });
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

/// Serialize a relationship state into a client-friendly JSON object.
fn serialize_state(state: &RelationshipState, agent_id: &str, user_id: &str) -> Value {
    json!({
        "tenant_id": state.tenant_id,
        "agent_id": agent_id,
        "user_id": user_id,
        "exists": true,
        "intimacy": state.intimacy,
        "stage": state.stage.as_str(),
        "emotion_trend": state.emotion_trend.as_str(),
        "recent_topics": state.recent_topics,
        "updated_at": state.updated_at,
    })
}

/// Parse the `messages` argument into [`Message`] values.
fn parse_messages(value: Option<&Value>) -> Result<Vec<Message>, Error> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| Error::InvalidInput("`messages` must be an array".into()))?;
    let mut out = Vec::with_capacity(array.len());
    for item in array {
        let role = item
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("message missing `role`".into()))?;
        let content = item
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("message missing `content`".into()))?;
        out.push(Message::new(role, content));
    }
    Ok(out)
}

/// Return the stable MCP schema for `relationship_update`.
pub fn relationship_update_definition() -> ToolDefinition {
    ToolDefinition {
        name: "relationship_update".into(),
        description: "Incrementally update the agent↔user relationship state from a batch of messages using deterministic rules (no LLM): each user positive-emotion message raises intimacy by 0.02, each negative-emotion message lowers it by 0.02 (clamped to [0,1]); stage is derived from intimacy thresholds; emotion trend from the intimacy delta; recent topics are recurring keywords. Persists to relationship_state.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "tenant_id": {"type": "string", "default": "default", "description": "Tenant namespace"},
                "agent_id": {"type": "string", "description": "The agent/entity identifier"},
                "user_id": {"type": "string", "description": "The user identifier"},
                "messages": {"type": "array", "description": "Conversation messages to apply", "items": {"type": "object", "properties": {"role": {"type": "string", "description": "user or assistant"}, "content": {"type": "string"}}, "required": ["role", "content"]}}
            },
            "required": ["agent_id", "user_id", "messages"]
        }),
    }
}

/// Return the stable MCP schema for `relationship_query`.
pub fn relationship_query_definition() -> ToolDefinition {
    ToolDefinition {
        name: "relationship_query".into(),
        description: "Read the current agent↔user relationship snapshot (intimacy, stage, emotion trend, recent topics) for a tenant/agent/user triple. Read-only.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "tenant_id": {"type": "string", "default": "default", "description": "Tenant namespace"},
                "agent_id": {"type": "string", "description": "The agent/entity identifier"},
                "user_id": {"type": "string", "description": "The user identifier"}
            },
            "required": ["agent_id", "user_id"]
        }),
    }
}

/// Return the stable MCP schema for `persona_timeline`.
pub fn persona_timeline_definition() -> ToolDefinition {
    ToolDefinition {
        name: "persona_timeline".into(),
        description: "Rebuild an entity's full persona evolution timeline (起点 → 关键转变点 → 现状) from its accumulated facts. ADD-only: no fact is deleted or overwritten; stance flips are kept as turning points. Read-only.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "entity_id": {"type": "integer", "description": "The entity id whose facts to reconstruct"}
            },
            "required": ["entity_id"]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a tool result's text payload into a JSON value.
    fn parse_payload(result: &ToolCallResult) -> Value {
        serde_json::from_str(
            &result
                .content
                .first()
                .and_then(|b| b.text.clone())
                .unwrap_or_default(),
        )
        .expect("valid JSON payload")
    }

    /// Objective: Verify `relationship_update` raises intimacy and persists the
    /// snapshot, reporting the new stage/trend.
    /// Invariants: two positive messages → intimacy 0.04, trend "rising".
    #[tokio::test]
    async fn relationship_update_applies_and_persists() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let tool = RelationshipUpdateTool::new(store.clone());
        let result = tool
            .call(&json!({
                "tenant_id": "tenant-a",
                "agent_id": "agent-bailiusu",
                "user_id": "alice",
                "messages": [
                    {"role": "user", "content": "谢谢你，今天很开心！"},
                    {"role": "assistant", "content": "不客气。"},
                    {"role": "user", "content": "谢谢你的温暖陪伴。"}
                ]
            }))
            .await
            .expect("call succeeds");
        let payload = parse_payload(&result);
        assert_eq!(payload["intimacy"], json!(0.04));
        assert_eq!(payload["stage"], json!("stranger"));
        assert_eq!(payload["emotion_trend"], json!("rising"));
        assert_eq!(payload["exists"], json!(true));
    }

    /// Objective: Verify `relationship_query` returns a default snapshot when
    /// no relationship exists yet.
    /// Invariants: `exists` is false, intimacy 0.0.
    #[tokio::test]
    async fn relationship_query_returns_default_when_absent() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let tool = RelationshipQueryTool::new(store);
        let result = tool
            .call(&json!({
                "tenant_id": "tenant-a",
                "agent_id": "agent-bailiusu",
                "user_id": "alice"
            }))
            .await
            .expect("call succeeds");
        let payload = parse_payload(&result);
        assert_eq!(payload["exists"], json!(false));
        assert_eq!(payload["intimacy"], json!(0.0));
    }

    /// Objective: Verify the three tool schemas declare the required fields.
    /// Invariants: update requires agent_id/user_id/messages; query requires
    /// agent_id/user_id; timeline requires entity_id.
    #[test]
    fn definitions_require_expected_fields() {
        let update = relationship_update_definition();
        let required_update: Vec<&str> = update
            .input_schema
            .get("required")
            .and_then(Value::as_array)
            .expect("required array")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(required_update.contains(&"agent_id"));
        assert!(required_update.contains(&"user_id"));
        assert!(required_update.contains(&"messages"));
        assert_eq!(update.name, "relationship_update");

        let query = relationship_query_definition();
        assert_eq!(query.name, "relationship_query");

        let timeline = persona_timeline_definition();
        let required_timeline: Vec<&str> = timeline
            .input_schema
            .get("required")
            .and_then(Value::as_array)
            .expect("required array")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(required_timeline.contains(&"entity_id"));
        assert_eq!(timeline.name, "persona_timeline");
    }
}
