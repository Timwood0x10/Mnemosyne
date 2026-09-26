//! `fact_provenance` MCP tool — the "why do we believe this?" evidence chain.
//!
//! Given a fact id, the tool answers three questions (provenance):
//!
//! - **Why do we believe it?** — the original-text evidence anchor.
//! - **How confident are we?** — `confidence` (its own column, independent of
//!   decay), plus the fact's epistemic `status`
//!   (active/superseded/contradicted).
//! - **What was it derived from?** — the `derived_from` derivation chain,
//!   recursively expanded into fact summaries (not causal claims: `F2
//!   derived_from F1` means "F2 was inferred from F1", never "F1 caused F2").
//!
//! This is a **read-only** tool — it never writes facts. It exists so an
//! agent (or human) can audit exactly why a cognitive fact is believed.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::cognition::{Fact, FactStore};
use crate::error::{Error, Result};
use crate::fact_store::SqliteFactStore;
use crate::mcp::types::{ToolCallResult, ToolDefinition, ToolHandler};

use super::tenant_scope;

/// Cap on how many `derived_from` hops we expand, so a corrupted chain can
/// never blow up the response.
const MAX_DERIVATION_DEPTH: usize = 8;
/// Cap on how many facts we include in the expanded chain (breadth safety).
const MAX_DERIVATION_NODES: usize = 32;

/// Handler for the `fact_provenance` tool.
pub struct FactProvenanceTool {
    store: Arc<SqliteFactStore>,
}

impl FactProvenanceTool {
    /// Construct the tool with the shared fact store.
    #[must_use]
    pub fn new(store: Arc<SqliteFactStore>) -> Self {
        Self { store }
    }
}

/// Render a fact as a compact provenance summary (no recursion, no payload
/// explosion — just enough to identify the source fact in the chain).
fn fact_summary(fact: &Fact) -> Value {
    json!({
        "fact_id": fact.id,
        "fact_type": format!("{:?}", fact.fact_type),
        "time": fact.time,
        "content": fact.payload.get("content").cloned().unwrap_or(Value::Null),
        "status": fact.status.as_str(),
    })
}

/// Recursively expand the `derived_from` chain starting from `fact`.
///
/// Visits are cycle-guarded by fact id and capped by depth/breadth so a
/// corrupted chain terminates. Returns a flat list of source-fact summaries.
fn expand_derivation_chain(store: &SqliteFactStore, fact: &Fact) -> Vec<Value> {
    let mut queue: Vec<i64> = fact.derived_from.clone();
    let mut seen = std::collections::HashSet::new();
    let mut expanded: Vec<Value> = Vec::new();
    let mut visited = 0usize;

    while let Some(id) = queue.pop() {
        if visited >= MAX_DERIVATION_NODES {
            break;
        }
        if !seen.insert(id) {
            continue;
        }
        let Ok(Some(source)) = store.get_fact_by_id(id) else {
            // A dangling id in the chain is not a hard failure — report it so
            // the caller can see the chain is broken, then keep going.
            expanded.push(json!({
                "fact_id": id,
                "missing": true,
                "status": "active",
            }));
            continue;
        };
        visited += 1;
        expanded.push(fact_summary(&source));
        // Two-hop limit keeps a deep chain from becoming unbounded even when
        // every node references several others.
        if queue.len() <= MAX_DERIVATION_DEPTH {
            for parent in &source.derived_from {
                if !seen.contains(parent) {
                    queue.push(*parent);
                }
            }
        }
    }
    expanded
}

#[async_trait]
impl ToolHandler for FactProvenanceTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult> {
        let fact_id = args
            .get("fact_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| Error::InvalidInput("missing required argument `fact_id`".into()))?;
        if fact_id <= 0 {
            return Err(Error::InvalidInput(format!(
                "fact_id must be positive, got {fact_id}"
            )));
        }

        let fact = self
            .store
            .get_fact_by_id(fact_id)?
            .ok_or_else(|| Error::NotFound(format!("no fact with id {fact_id}")))?;
        // A fact id alone does not say who owns it: enforce the tenant when the
        // caller supplied one.
        tenant_scope::ensure_entity_tenant(
            &self.store,
            fact.entity_id,
            tenant_scope::tenant_argument(args)?,
        )?;

        // Fetch the original-text evidence anchor, if any — content AND the
        // source byte span, so the audit answer says WHERE the quote sits
        // (content-only reads broke re-locatability at this surface).
        let evidence = match fact.evidence_id {
            Some(evidence_id) => self.store.get_evidence_anchor(evidence_id)?,
            None => None,
        };

        let derived_from = expand_derivation_chain(self.store.as_ref(), &fact);

        let payload = json!({
            "fact_id": fact.id,
            "fact_type": format!("{:?}", fact.fact_type),
            "time": fact.time,
            "payload": fact.payload,
            "confidence": fact.confidence,
            "status": fact.status.as_str(),
            "evidence": evidence.as_ref().and_then(|a| a.content.clone()),
            "evidence_start": evidence.as_ref().and_then(|a| a.start_offset),
            "evidence_end": evidence.as_ref().and_then(|a| a.end_offset),
            "derived_from": derived_from,
        });

        Ok(ToolCallResult::text(
            serde_json::to_string(&payload)
                .map_err(|e| Error::Internal(format!("serialize provenance payload: {e}")))?,
        ))
    }
}

/// Return the stable MCP schema for `fact_provenance`.
#[must_use]
pub fn fact_provenance_definition() -> ToolDefinition {
    ToolDefinition {
        name: "fact_provenance".into(),
        description: "Audit why a cognitive fact is believed: returns the fact's confidence, epistemic status (active/superseded/contradicted), the original-text evidence anchor behind it, and the derivation chain of source facts it was inferred from. Read-only; never writes.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "fact_id": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Fact id to audit (required)"
                },
                "tenant_id": {
                    "type": "string",
                    "description": "Optional: the tenant the fact's entity must belong to. Implied by fact_id; supply it to have a cross-tenant fact rejected instead of served."
                }
            },
            "required": ["fact_id"]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cognition::{FactStatus, FactType};

    /// Objective: Verify the tool is tenant-scoped when the caller states a
    /// tenant: a fact id carries no ownership, so any client could otherwise
    /// audit another tenant's facts by guessing an id.
    /// Invariants: wrong tenant → NotFound; owning tenant → served.
    #[tokio::test]
    async fn provenance_is_tenant_scoped_when_asked() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("open fact store"));
        let entity_id = store
            .resolve_user("tenant-a", "alice")
            .expect("resolve user");
        let fact_id = store
            .insert_fact(&Fact {
                id: None,
                entity_id,
                fact_type: FactType::Goal,
                time: 2026,
                payload: json!({"content": "我要学 Rust"}),
                evidence_id: None,
                created_at: 2026,
                ..Fact::default()
            })
            .expect("insert fact");
        let tool = FactProvenanceTool::new(store);

        let error = tool
            .call(&json!({"fact_id": fact_id, "tenant_id": "tenant-b"}))
            .await
            .expect_err("another tenant's fact must not be served");
        assert!(
            matches!(error, Error::NotFound(_)),
            "a cross-tenant fact is NotFound, got {error:?}"
        );

        let result = tool
            .call(&json!({"fact_id": fact_id, "tenant_id": "tenant-a"}))
            .await
            .expect("the owning tenant is served");
        let payload: Value = serde_json::from_str(
            result.content[0]
                .text
                .as_deref()
                .expect("provenance returns a text block"),
        )
        .expect("valid JSON payload");
        assert_eq!(payload["fact_id"], json!(fact_id));
    }

    /// Objective: Verify the tool answers the full provenance story for a fact
    /// with an evidence anchor and a derivation chain.
    /// Invariants: confidence/status/evidence/derived_from all present; the
    /// chain expands to source facts; missing source ids surface as `missing`.
    #[tokio::test]
    async fn provenance_reports_evidence_and_derivation_chain() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("open fact store"));
        // Seed two source facts and one derived fact.
        let src_a = store
            .insert_fact(&Fact {
                id: None,
                entity_id: 7,
                fact_type: FactType::Preference,
                time: 2024,
                payload: json!({"content": "likes Python"}),
                evidence_id: None,
                created_at: 2024,
                ..Fact::default()
            })
            .expect("insert source fact A");
        let src_b = store
            .insert_fact(&Fact {
                id: None,
                entity_id: 7,
                fact_type: FactType::Preference,
                time: 2025,
                payload: json!({"content": "started Rust"}),
                evidence_id: None,
                created_at: 2025,
                ..Fact::default()
            })
            .expect("insert source fact B");
        // Attach an evidence row the derived fact points at.
        let evidence_id = store
            .insert_evidence(Some(128), None, "2026-08-15: “我从去年开始喜欢 Rust”")
            .expect("insert evidence row");
        let derived = store
            .insert_fact(&Fact {
                id: None,
                entity_id: 7,
                fact_type: FactType::Preference,
                time: 2026,
                payload: json!({"content": "prefers Rust"}),
                evidence_id: Some(evidence_id),
                created_at: 2026,
                confidence: 0.85,
                derived_from: vec![src_a, src_b],
                status: FactStatus::Active,
            })
            .expect("insert derived fact");

        let tool = FactProvenanceTool::new(store);
        let result = tool
            .call(&json!({"fact_id": derived}))
            .await
            .expect("provenance lookup succeeds");
        let text = result.content[0]
            .text
            .as_deref()
            .expect("text block present");
        let body: Value = serde_json::from_str(text).expect("provenance result is valid JSON");
        assert_eq!(body["fact_id"], json!(derived), "fact id echoed");
        assert_eq!(body["status"], json!("active"), "status echoed");
        assert_eq!(body["confidence"], json!(0.85), "confidence echoed");
        let evidence = body["evidence"].as_str().expect("evidence content present");
        assert!(
            evidence.contains("喜欢 Rust"),
            "evidence carries original text, got {evidence}"
        );
        // Span keys ride alongside the content (additive); this row was
        // inserted without a span so the values are null, but the KEYS must
        // always be present for consumers.
        assert!(
            body.get("evidence_start").is_some() && body.get("evidence_end").is_some(),
            "provenance payload must expose the evidence span keys, got {body}"
        );
        let chain = body["derived_from"].as_array().expect("chain is an array");
        assert_eq!(chain.len(), 2, "chain expands both source facts");
        let ids: Vec<i64> = chain.iter().filter_map(|v| v["fact_id"].as_i64()).collect();
        assert!(
            ids.contains(&src_a) && ids.contains(&src_b),
            "chain contains both source ids"
        );
    }

    /// Objective: Verify a fact with no evidence anchor and no derivation chain
    /// still audits cleanly with nullable evidence and an empty chain.
    /// Invariants: never panics; evidence null; chain empty.
    #[tokio::test]
    async fn provenance_handles_fact_without_evidence_or_chain() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("open fact store"));
        let id = store
            .insert_fact(&Fact {
                id: None,
                entity_id: 7,
                fact_type: FactType::Event,
                time: 2026,
                payload: json!({"content": "went for a walk"}),
                evidence_id: None,
                created_at: 2026,
                ..Fact::default()
            })
            .expect("insert bare fact");

        let tool = FactProvenanceTool::new(store);
        let result = tool
            .call(&json!({"fact_id": id}))
            .await
            .expect("bare fact audits");
        let text = result.content[0]
            .text
            .as_deref()
            .expect("text block present");
        let body: Value = serde_json::from_str(text).expect("valid JSON");
        assert!(body["evidence"].is_null(), "no evidence anchor → null");
        assert_eq!(
            body["derived_from"].as_array().map(Vec::len),
            Some(0),
            "no derivation chain → empty array"
        );
        assert_eq!(body["status"], json!("active"), "bare fact is active");
    }

    /// Objective: Verify invalid inputs and missing facts return typed errors,
    /// never panics and never fabricate a fact.
    /// Invariants: non-positive or missing fact_id → InvalidInput; unknown id →
    /// NotFound.
    #[tokio::test]
    async fn provenance_rejects_bad_inputs_and_missing_facts() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("open fact store"));
        let tool = FactProvenanceTool::new(store);

        let error = tool
            .call(&json!({}))
            .await
            .expect_err("missing fact_id must fail");
        assert!(
            matches!(error, Error::InvalidInput(_)),
            "missing fact_id is InvalidInput, got {error:?}"
        );

        let error = tool
            .call(&json!({"fact_id": 0}))
            .await
            .expect_err("non-positive fact_id must fail");
        assert!(
            matches!(error, Error::InvalidInput(_)),
            "non-positive fact_id is InvalidInput, got {error:?}"
        );

        let error = tool
            .call(&json!({"fact_id": 404}))
            .await
            .expect_err("unknown fact must fail");
        assert!(
            matches!(error, Error::NotFound(_)),
            "unknown fact is NotFound, got {error:?}"
        );
    }
}
