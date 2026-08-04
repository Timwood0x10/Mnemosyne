//! `story_bridge` MCP tool — bridge a novel protagonist's story events into
//! fact-store persona facts so `persona_timeline` works on novel corpus.
//!
//! Novels compile into the knowledge graph (`Object`/`Edge`); the persona
//! timeline reads the fact store. This tool connects the two: it walks a
//! character's `participated_in` story events, re-expresses each event's text
//! as a persona signal, and writes the resulting facts onto a fact-store
//! entity. After a bridge, `persona_timeline(entity_id)` yields the same
//! `起点 → 转折 → 现状` arc the live-conversation path already produces.

use std::sync::Arc;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::fact_store::SqliteFactStore;
use crate::knowledge::SQLiteKnowledgeStore;
use crate::mcp::types::{ToolCallResult, ToolDefinition, ToolHandler};
use crate::story_bridge::{BridgeStats, bridge_story_events_to_persona};

/// Handler for the `story_bridge` tool.
pub struct StoryBridgeTool {
    kstore: Arc<SQLiteKnowledgeStore>,
    fstore: Arc<SqliteFactStore>,
}

impl StoryBridgeTool {
    /// Create the tool with the shared knowledge + fact stores.
    #[must_use]
    pub fn new(kstore: Arc<SQLiteKnowledgeStore>, fstore: Arc<SqliteFactStore>) -> Self {
        Self { kstore, fstore }
    }
}

#[async_trait::async_trait]
impl ToolHandler for StoryBridgeTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult> {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing required argument `name`".into()))?;
        let tenant_id = args
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("default");

        let stats: BridgeStats = bridge_story_events_to_persona(
            self.kstore.as_ref(),
            self.fstore.as_ref(),
            tenant_id,
            name,
        )
        .await?;
        let payload = serde_json::to_value(&stats)
            .map_err(|e| Error::Internal(format!("serialize story-bridge result: {e}")))?;
        Ok(ToolCallResult::text(
            serde_json::to_string(&payload)
                .map_err(|e| Error::Internal(format!("serialize payload: {e}")))?,
        ))
    }
}

/// Return the stable MCP schema for `story_bridge`.
#[must_use]
pub fn story_bridge_definition() -> ToolDefinition {
    ToolDefinition {
        name: "story_bridge".into(),
        description: "Bridge a novel character's story events (from the knowledge graph) into fact-store persona facts so persona_timeline can output their evolution (起点→转折→现状). Returns the fact-store entity_id for the character.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Character name, e.g. '流苏' (required)"
                },
                "tenant_id": {
                    "type": "string",
                    "description": "Tenant namespace for the fact-store entity (default 'default')"
                }
            },
            "required": ["name"]
        }),
    }
}
