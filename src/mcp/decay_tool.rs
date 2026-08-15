//! `memory_decay` MCP tool — deterministic forgetting management.
//!
//! Scans facts and writes back decay scores / archive flags using the
//! deterministic policy in [`crate::decay`]. Decay never deletes a fact, so the
//! persona evolution timeline stays reconstructable. High-value persona facts
//! (`agent_personality` attribution, `Relationship`) are protected unless
//! `force` is set. No LLM is involved.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::decay::{DecayConfig, DecayStrategy, run_decay_pass};
use crate::error::Error;
use crate::fact_store::SqliteFactStore;
use crate::mcp::types::{ToolCallResult, ToolDefinition, ToolHandler};

/// Handler for the `memory_decay` tool.
pub struct MemoryDecayTool {
    fact_store: Arc<SqliteFactStore>,
    config: DecayConfig,
}

impl MemoryDecayTool {
    /// Build the tool with the shared fact store and the configured decay policy.
    pub fn new(fact_store: Arc<SqliteFactStore>) -> Self {
        Self {
            fact_store,
            config: DecayConfig::load(),
        }
    }

    /// Build the tool with an explicit decay policy (used by tests).
    pub fn with_config(fact_store: Arc<SqliteFactStore>, config: DecayConfig) -> Self {
        Self { fact_store, config }
    }
}

#[async_trait]
impl ToolHandler for MemoryDecayTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let tenant_id = args
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let entity_id = args.get("entity_id").and_then(Value::as_i64);
        let strategy = args
            .get("strategy")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);

        let mut config = self.config.clone();
        if let Some(strategy) = strategy {
            config.strategy = strategy
                .parse::<DecayStrategy>()
                .map_err(Error::InvalidInput)?;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Error::Internal(e.to_string()))?
            .as_secs() as i64;

        // Scope the sweep to the requested tenant: without this, an omitted
        // entity_id made the tool decay facts across EVERY tenant in the
        // database, ignoring the advertised tenant namespace.
        let stats = run_decay_pass(
            self.fact_store.as_ref(),
            &config,
            entity_id,
            Some(tenant_id),
            force,
            now,
        )?;

        let payload = json!({
            "scanned": stats.scanned,
            "archived": stats.archived,
            "kept": stats.kept,
            "high_value_protected": stats.high_value_protected,
        });
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

/// Return the stable MCP schema for `memory_decay`.
pub fn memory_decay_definition() -> ToolDefinition {
    ToolDefinition {
        name: "memory_decay".into(),
        description: "Deterministic memory decay/forgetting: down-weight and archive stale facts (time/importance/access-frequency). Never deletes facts, so the persona evolution timeline stays reconstructable. High-value persona facts (agent_personality, Relationship) are protected unless force=true. No LLM.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "tenant_id": {"type": "string", "default": "default", "description": "Tenant namespace for the facts scanned"},
                "entity_id": {"type": "integer", "description": "Entity id to decay; omit to scan all entities in the tenant"},
                "strategy": {"type": "string", "enum": ["time_based", "importance_based", "access_frequency_based", "hybrid"], "description": "Override the configured decay strategy"},
                "force": {"type": "boolean", "default": false, "description": "Ignore high-value persona protection and decay protected facts too"}
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_personality::AGENT_PERSONALITY_ATTRIBUTION;
    use crate::cognition::{Fact, FactStore, FactType};
    use serde_json::json;

    fn fact(
        id: Option<i64>,
        entity_id: i64,
        fact_type: FactType,
        created_at: i64,
        payload: Value,
    ) -> Fact {
        Fact {
            id,
            entity_id,
            fact_type,
            time: 1,
            payload,
            evidence_id: None,
            created_at,
            ..Fact::default()
        }
    }

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

    /// Objective: Verify the tool call reports scan statistics — it archives
    /// old decayable facts and counts high-value persona facts as protected.
    /// Invariants: scanned == total, archived == decayed count, kept ==
    /// protected count, high_value_protected == protected count.
    #[tokio::test]
    async fn memory_decay_reports_statistics() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let eid = store
            .resolve_user("tenant-a", "alice")
            .expect("resolve user");
        // Two old events (decayable) and one old persona fact (protected).
        store
            .insert_fact(&fact(
                None,
                eid,
                FactType::Event,
                0,
                json!({"importance": 0.9}),
            ))
            .expect("insert old event");
        store
            .insert_fact(&fact(
                None,
                eid,
                FactType::Event,
                0,
                json!({"importance": 0.9}),
            ))
            .expect("insert old event");
        store
            .insert_fact(&fact(
                None,
                eid,
                FactType::Preference,
                0,
                json!({"attribution": AGENT_PERSONALITY_ATTRIBUTION}),
            ))
            .expect("insert persona fact");

        let tool = MemoryDecayTool::with_config(store.clone(), DecayConfig::default());
        let result = tool
            .call(&json!({ "entity_id": eid }))
            .await
            .expect("call succeeds");
        let payload = parse_payload(&result);

        assert_eq!(payload["scanned"], json!(3), "all facts scanned");
        assert_eq!(payload["archived"], json!(2), "both old events archived");
        assert_eq!(payload["kept"], json!(1), "protected persona fact kept");
        assert_eq!(
            payload["high_value_protected"],
            json!(1),
            "persona fact counted as protected"
        );
    }

    /// Objective: Verify `force=true` archives a protected persona fact too,
    /// and that the fact remains readable (never deleted).
    /// Invariants: archived == 1, high_value_protected == 0, row still present.
    #[tokio::test]
    async fn force_archives_protected_persona_fact() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let eid = store
            .resolve_user("tenant-a", "alice")
            .expect("resolve user");
        store
            .insert_fact(&fact(
                None,
                eid,
                FactType::Preference,
                0,
                json!({"attribution": AGENT_PERSONALITY_ATTRIBUTION}),
            ))
            .expect("insert persona fact");

        let tool = MemoryDecayTool::with_config(store.clone(), DecayConfig::default());
        let result = tool
            .call(&json!({ "entity_id": eid, "force": true }))
            .await
            .expect("call succeeds");
        let payload = parse_payload(&result);

        assert_eq!(
            payload["archived"],
            json!(1),
            "force archives the persona fact"
        );
        assert_eq!(
            payload["high_value_protected"],
            json!(0),
            "no protection under force"
        );

        // Still readable — decay never deletes.
        let archived = store.list_archived(eid).expect("list archived");
        assert_eq!(archived.len(), 1, "archived fact remains readable");
    }

    /// Objective: Verify the tool scans all entities in the REQUESTED tenant
    /// when `entity_id` is absent — the sweep is tenant-scoped, so entities in
    /// other tenants are untouched.
    /// Invariants: tenant-a holds two decayable facts → scanned == 2, both
    /// archived; tenant-b facts are not scanned.
    #[tokio::test]
    async fn memory_decay_scans_all_entities_when_entity_id_absent() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let a = store
            .resolve_user("tenant-a", "alice")
            .expect("resolve alice");
        let b = store.resolve_user("tenant-a", "bob").expect("resolve bob");
        store
            .insert_fact(&fact(
                None,
                a,
                FactType::Event,
                0,
                json!({"importance": 0.9}),
            ))
            .expect("insert alice fact");
        store
            .insert_fact(&fact(
                None,
                b,
                FactType::Event,
                0,
                json!({"importance": 0.9}),
            ))
            .expect("insert bob fact");
        // A tenant-b entity must stay out of the sweep.
        let c = store
            .resolve_user("tenant-b", "carol")
            .expect("resolve carol");
        store
            .insert_fact(&fact(
                None,
                c,
                FactType::Event,
                0,
                json!({"importance": 0.9}),
            ))
            .expect("insert carol fact");

        let tool = MemoryDecayTool::with_config(store, DecayConfig::default());
        let result = tool
            .call(&json!({ "tenant_id": "tenant-a" }))
            .await
            .expect("call succeeds");
        let payload = parse_payload(&result);

        assert_eq!(
            payload["scanned"],
            json!(2),
            "only tenant-a entities scanned"
        );
        assert_eq!(
            payload["archived"],
            json!(2),
            "both tenant-a facts archived"
        );
    }

    /// Objective: Verify the definition exposes the decay tool schema.
    /// Invariants: name is `memory_decay`, strategy enum present.
    #[test]
    fn definition_exposes_memory_decay_schema() {
        let def = memory_decay_definition();
        assert_eq!(def.name, "memory_decay");
        let strategies = def
            .input_schema
            .get("properties")
            .and_then(|p| p.get("strategy"))
            .and_then(|s| s.get("enum"))
            .and_then(Value::as_array)
            .expect("strategy enum");
        assert!(
            strategies.iter().any(|v| v == "hybrid"),
            "hybrid strategy listed"
        );
    }
}
