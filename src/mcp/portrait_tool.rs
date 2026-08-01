//! `portrait_extract` MCP tool — deterministic resume/person portrait.
//!
//! Wraps [`crate::knowledge::portrait::PortraitExtractor`] so an agent can
//! turn resume/人物文档 plain text into a structured [`PersonPortrait`]
//! (name/position/contacts/links/skills/projects) without an LLM. This is the
//! "document → portrait" path of the external-knowledge plan, complementary to
//! the cognition-Facts pipeline (which targets conversations, not narratives).

use serde_json::Value;

use crate::error::Error;
use crate::knowledge::portrait::{PortraitError, PortraitExtractor};
use crate::mcp::types::{ToolCallResult, ToolDefinition, ToolHandler};

/// Handler for the `portrait_extract` tool.
pub struct PortraitTool {
    extractor: PortraitExtractor,
}

impl PortraitTool {
    /// Create the tool with a fresh stateless extractor.
    #[must_use]
    pub fn new() -> Self {
        Self {
            extractor: PortraitExtractor::new(),
        }
    }
}

impl Default for PortraitTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ToolHandler for PortraitTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing required argument `text`".into()))?;
        let source_name = args
            .get("source_name")
            .and_then(Value::as_str)
            .unwrap_or("resume");

        match self.extractor.extract(text) {
            Ok(portrait) => {
                let payload = serde_json::json!({
                    "source_name": source_name,
                    "extracted_chars": text.chars().count(),
                    "portrait": portrait,
                });
                Ok(ToolCallResult::text(
                    serde_json::to_string(&payload)
                        .unwrap_or_else(|e| format!("{{\"error\": \"serialize portrait: {e}\"}}")),
                ))
            }
            Err(e) => Ok(match e {
                PortraitError::EmptyInput => ToolCallResult::error(
                    "portrait extraction failed: input is empty (no extractable lines)",
                ),
                PortraitError::MissingProjects => ToolCallResult::error(
                    "portrait extraction failed: no project section marker (`—`) found",
                ),
            }),
        }
    }
}

/// Return the stable MCP schema for `portrait_extract`.
#[must_use]
pub fn portrait_extract_definition() -> ToolDefinition {
    ToolDefinition {
        name: "portrait_extract".into(),
        description: "Extract a structured person portrait (name/position/contacts/links/skills/projects) from resume or person-document plain text using deterministic rules. Feed the output of knowledge_ingest or pdf extraction here.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Plain text of the resume/person document (required)"
                },
                "source_name": {
                    "type": "string",
                    "description": "Optional source label for provenance (default 'resume')"
                }
            },
            "required": ["text"]
        }),
    }
}
