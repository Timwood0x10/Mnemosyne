//! `person_key_events` MCP tool — distill a person's full trajectory into
//! key events with evidence anchors.
//!
//! Per the key-events plan: trajectory nodes are **key events** (not
//! chapters), each with a transparent importance score (evidence richness +
//! network centrality + turning-point flag) and original-text evidence. The
//! tool **supplies evidence**; the AI consuming this tool performs the
//! personality/turning analysis — this handler never emits conclusions.

use std::sync::Arc;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::knowledge::SQLiteKnowledgeStore;
use crate::knowledge::key_events::{KEY_EVENT_THRESHOLD, extract_key_events};
use crate::mcp::types::{ToolCallResult, ToolDefinition, ToolHandler};

/// Handler for the `person_key_events` tool.
pub struct KeyEventsTool {
    store: Arc<SQLiteKnowledgeStore>,
}

impl KeyEventsTool {
    /// Create the tool with the shared knowledge store.
    #[must_use]
    pub fn new(store: Arc<SQLiteKnowledgeStore>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl ToolHandler for KeyEventsTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult> {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing required argument `name`".into()))?;
        let doc_title = args.get("doc_title").and_then(Value::as_str);
        let threshold = args
            .get("threshold")
            .and_then(Value::as_f64)
            .unwrap_or(KEY_EVENT_THRESHOLD)
            .clamp(0.0, 1.0);

        let result = extract_key_events(self.store.as_ref(), name, doc_title, threshold).await?;
        let payload = serde_json::to_value(&result)
            .map_err(|e| Error::Internal(format!("serialize key-events result: {e}")))?;
        Ok(ToolCallResult::text(
            serde_json::to_string(&payload)
                .map_err(|e| Error::Internal(format!("serialize payload: {e}")))?,
        ))
    }
}

/// Return the stable MCP schema for `person_key_events`.
#[must_use]
pub fn key_events_definition() -> ToolDefinition {
    ToolDefinition {
        name: "person_key_events".into(),
        description: "Distill a person's full trajectory into key events: each carries a transparent importance score (evidence richness + participant centrality + turning-point flag) and original-text evidence anchors. The tool supplies evidence only — use it to inform your own personality/turning analysis.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Entity/person name (required)"
                },
                "doc_title": {
                    "type": "string",
                    "description": "Optional document title filter, e.g. '水浒传'"
                },
                "threshold": {
                    "type": "number",
                    "minimum": 0.0,
                    "maximum": 1.0,
                    "description": "Importance threshold for key events (default 0.7)"
                }
            },
            "required": ["name"]
        }),
    }
}
