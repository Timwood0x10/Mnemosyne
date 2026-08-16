//! Decision MCP tools (v0.3.1, experimental) — the "why did the agent act
//! this way?" layer.
//!
//! Two tools, no more (per `plan/cognitive-state-v03.md`):
//!
//! - `decision_trace` — trace a decision back to the facts that supported it
//!   (`because`), each fact to its evidence. Supporting evidence, not
//!   causality: "D was supported by F17", never "F17 caused D".
//! - `decision_search` — lightweight keyword search over a subject's
//!   decisions (`verb`/`object`). Decisions deliberately do not depend on the
//!   embedding-based retrieval engine.
//!
//! Both tools are read-only. This module does NOT touch `memory_decay`
//! semantics: decisions follow their own entity lifecycle.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::cognition::FactStore;
use crate::decision::{Decision, DecisionOutcome};
use crate::error::{Error, Result};
use crate::fact_store::SqliteFactStore;
use crate::mcp::types::{ToolCallResult, ToolDefinition, ToolHandler};

/// Render a decision plus its resolved supporting facts into a client object.
fn decision_json(store: &SqliteFactStore, decision: &Decision) -> Result<Value> {
    let mut because_facts = Vec::new();
    for fact_id in &decision.because {
        match store.get_fact_by_id(*fact_id)? {
            Some(fact) => because_facts.push(json!({
                "fact_id": fact.id,
                "fact_type": format!("{:?}", fact.fact_type),
                "content": fact.payload.get("content").cloned().unwrap_or(Value::Null),
                "status": fact.status.as_str(),
            })),
            None => because_facts.push(json!({
                "fact_id": fact_id,
                "missing": true,
            })),
        }
    }
    Ok(json!({
        "decision_id": decision.id,
        "subject": decision.subject,
        "verb": decision.verb,
        "object": decision.object,
        "made_at": decision.made_at,
        "because": because_facts,
        "outcome": decision.outcome.map(DecisionOutcome::as_str),
        "status": decision.status.as_str(),
    }))
}

/// Shared argument parsing for both tools.
fn parse_subject(args: &Value) -> Result<i64> {
    args.get("subject")
        .and_then(Value::as_i64)
        .ok_or_else(|| Error::InvalidInput("missing required argument `subject`".into()))
}

/// Handler for the `decision_trace` tool.
pub struct DecisionTraceTool {
    store: Arc<SqliteFactStore>,
}

impl DecisionTraceTool {
    /// Construct the tool over the shared fact store.
    #[must_use]
    pub fn new(store: Arc<SqliteFactStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl ToolHandler for DecisionTraceTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult> {
        let decision_id = args
            .get("decision_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| Error::InvalidInput("missing required argument `decision_id`".into()))?;
        if decision_id <= 0 {
            return Err(Error::InvalidInput(format!(
                "decision_id must be positive, got {decision_id}"
            )));
        }
        let decision = self
            .store
            .get_decision(decision_id)?
            .ok_or_else(|| Error::NotFound(format!("no decision with id {decision_id}")))?;
        let payload = decision_json(self.store.as_ref(), &decision)?;
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

/// Handler for the `decision_search` tool.
pub struct DecisionSearchTool {
    store: Arc<SqliteFactStore>,
}

impl DecisionSearchTool {
    /// Construct the tool over the shared fact store.
    #[must_use]
    pub fn new(store: Arc<SqliteFactStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl ToolHandler for DecisionSearchTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult> {
        let subject = parse_subject(args)?;
        let keyword = match args.get("keyword") {
            Some(Value::Null) | None => String::new(),
            Some(value) => value
                .as_str()
                .ok_or_else(|| Error::InvalidInput("`keyword` must be a string".into()))?
                .to_string(),
        };
        let limit = match args.get("limit") {
            Some(Value::Null) | None => 10usize,
            Some(value) => value
                .as_u64()
                .ok_or_else(|| Error::InvalidInput("`limit` must be an integer".into()))?
                as usize,
        };
        let decisions = self.store.search_decisions(subject, &keyword)?;
        let mut payloads = Vec::new();
        for decision in decisions.into_iter().take(limit) {
            payloads.push(decision_json(self.store.as_ref(), &decision)?);
        }
        Ok(ToolCallResult::text(
            json!({ "decisions": payloads }).to_string(),
        ))
    }
}

/// Return the stable MCP schema for `decision_trace`.
#[must_use]
pub fn decision_trace_definition() -> ToolDefinition {
    ToolDefinition {
        name: "decision_trace".into(),
        description: "Trace a decision back to the facts that supported it (supporting evidence, not causality) and their statuses. Read-only.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "decision_id": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Decision id to trace (required)"
                }
            },
            "required": ["decision_id"]
        }),
    }
}

/// Return the stable MCP schema for `decision_search`.
#[must_use]
pub fn decision_search_definition() -> ToolDefinition {
    ToolDefinition {
        name: "decision_search".into(),
        description: "Search a subject's decisions by keyword over verb/object (case-insensitive), newest first. Lightweight keyword search; no embedding dependency. Read-only.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "subject": {
                    "type": "integer",
                    "description": "Entity id whose decisions to search (required)"
                },
                "keyword": {
                    "type": "string",
                    "description": "Keyword to match against verb/object; empty matches all"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "default": 10,
                    "description": "Maximum number of decisions to return"
                }
            },
            "required": ["subject"]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cognition::{Fact, FactType};
    use crate::decision::DecisionStatus;

    fn parse_payload(result: &ToolCallResult) -> Value {
        serde_json::from_str(
            &result
                .content
                .first()
                .and_then(|block| block.text.clone())
                .unwrap_or_default(),
        )
        .expect("valid JSON payload")
    }

    fn decision(
        subject: i64,
        verb: &str,
        object: &str,
        because: Vec<i64>,
    ) -> crate::decision::Decision {
        crate::decision::Decision {
            id: None,
            subject,
            verb: verb.to_string(),
            object: object.to_string(),
            made_at: 2026,
            because,
            outcome: None,
            status: DecisionStatus::Open,
        }
    }

    /// Objective: Verify `decision_trace` expands `because` into resolved
    /// facts, marking missing ids, and echoes outcome/status.
    /// Invariants: supporting facts resolved; missing fact flagged; outcome
    /// null for an open decision.
    #[tokio::test]
    async fn decision_trace_resolves_supporting_facts() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let fact_id = store
            .insert_fact(&Fact {
                id: None,
                entity_id: 7,
                fact_type: FactType::Preference,
                time: 2026,
                payload: json!({"content": "用户最近情绪低落"}),
                evidence_id: None,
                created_at: 2026,
                ..Fact::default()
            })
            .expect("insert supporting fact");
        let decision_id = store
            .insert_decision(&decision(7, "decline", "不给建议", vec![fact_id, 404]))
            .expect("insert decision");

        let tool = DecisionTraceTool::new(store);
        let result = tool
            .call(&json!({"decision_id": decision_id}))
            .await
            .expect("trace succeeds");
        let body = parse_payload(&result);
        assert_eq!(body["verb"], json!("decline"));
        assert_eq!(body["status"], json!("open"));
        assert!(body["outcome"].is_null(), "open decision has no outcome");
        let because = body["because"].as_array().expect("because array");
        assert_eq!(because.len(), 2, "both supporting ids reported");
        assert_eq!(
            because[0]["content"],
            json!("用户最近情绪低落"),
            "existing fact resolved with content"
        );
        assert_eq!(because[1]["missing"], json!(true), "missing id flagged");
    }

    /// Objective: Verify `decision_trace` rejects invalid input and unknown
    /// ids with typed errors.
    /// Invariants: missing/non-positive decision_id → InvalidInput; unknown id
    /// → NotFound.
    #[tokio::test]
    async fn decision_trace_rejects_bad_inputs() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let tool = DecisionTraceTool::new(store);

        let error = tool
            .call(&json!({}))
            .await
            .expect_err("missing decision_id must fail");
        assert!(
            matches!(error, Error::InvalidInput(_)),
            "missing decision_id is InvalidInput, got {error:?}"
        );

        let error = tool
            .call(&json!({"decision_id": 0}))
            .await
            .expect_err("non-positive decision_id must fail");
        assert!(
            matches!(error, Error::InvalidInput(_)),
            "non-positive decision_id is InvalidInput, got {error:?}"
        );

        let error = tool
            .call(&json!({"decision_id": 9999}))
            .await
            .expect_err("unknown decision must fail");
        assert!(
            matches!(error, Error::NotFound(_)),
            "unknown decision is NotFound, got {error:?}"
        );
    }

    /// Objective: Verify `decision_search` matches keywords, applies limit,
    /// and returns newest-first.
    /// Invariants: matching decisions returned; limit caps results; subject
    /// isolation holds; empty keyword matches all.
    #[tokio::test]
    async fn decision_search_filters_by_keyword_and_limit() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let mut old = decision(7, "promise", "陪用户明天去医院", vec![]);
        old.made_at = 2024;
        store.insert_decision(&old).expect("insert old");
        store
            .insert_decision(&decision(7, "decide", "这周末学习 Rust", vec![]))
            .expect("insert decide");
        store
            .insert_decision(&decision(8, "promise", "陪用户去医院", vec![]))
            .expect("insert other subject");

        let tool = DecisionSearchTool::new(store);
        let result = tool
            .call(&json!({"subject": 7, "keyword": "医院", "limit": 10}))
            .await
            .expect("search succeeds");
        let body = parse_payload(&result);
        let hits = body["decisions"].as_array().expect("decisions array");
        assert_eq!(hits.len(), 1, "one match in subject 7");
        assert_eq!(hits[0]["object"], json!("陪用户明天去医院"));

        let result = tool
            .call(&json!({"subject": 7, "keyword": "", "limit": 1}))
            .await
            .expect("search all succeeds");
        let body = parse_payload(&result);
        let hits = body["decisions"].as_array().expect("decisions array");
        assert_eq!(hits.len(), 1, "limit caps the result count");
        assert_eq!(hits[0]["object"], json!("这周末学习 Rust"), "newest first");
    }

    /// Objective: Verify the tool schemas declare their required arguments.
    /// Invariants: decision_trace requires decision_id; decision_search
    /// requires subject.
    #[test]
    fn decision_definitions_declare_contracts() {
        let trace = decision_trace_definition();
        assert_eq!(trace.name, "decision_trace");
        let trace_required: Vec<&str> = trace
            .input_schema
            .get("required")
            .and_then(Value::as_array)
            .expect("required array")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(trace_required.contains(&"decision_id"));

        let search = decision_search_definition();
        assert_eq!(search.name, "decision_search");
        let search_required: Vec<&str> = search
            .input_schema
            .get("required")
            .and_then(Value::as_array)
            .expect("required array")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(search_required.contains(&"subject"));
    }
}
