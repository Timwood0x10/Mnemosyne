//! `persona_inject` MCP tool — the "人设不崩" injection feed.
//!
//! Given an agent (and an optional tenant), the tool resolves the agent entity
//! and aggregates its stored `agent_personality` facts into a structured
//! [`PersonaCard`] (see [`crate::persona::inject`]), optionally merged with a
//! hand-authored JSON persona card. The card is returned as a JSON object or
//! as a text block ready to be pasted into a system prompt.
//!
//! This is a **deterministic, no-LLM** tool: every field is produced by
//! grouping stored facts by type, never by a generative model. It is
//! **read-only** — it never writes facts. The JSON card file is optional; when
//! absent (or lacking an entry for the requested pair) the tool falls back to
//! the fact-aggregated card alone.
//!
//! Multi-tenant, multi-agent: the `(tenant_id, agent_id)` pair selects a
//! distinct card, so a novel with many characters or a companion dialog with
//! several agents each gets its own stable persona.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::cognition::FactStore;
use crate::config::resolve_resource_path;
use crate::error::Error;
use crate::fact_store::SqliteFactStore;
use crate::mcp::types::{ToolCallResult, ToolDefinition, ToolHandler};
use crate::persona::inject::{
    PersonaCard, build_persona_card_from_facts, load_persona_cards, lookup_persona_card,
    merge_persona_card_file, persona_card_to_text,
};

/// Handler for the `persona_inject` tool.
pub struct PersonaInjectTool {
    fact_store: Arc<SqliteFactStore>,
    cards_path: String,
}

impl PersonaInjectTool {
    /// Construct the tool with the shared fact store.
    ///
    /// The persona card JSON path is `config/persona_cards.json` under the
    /// runtime resource root.
    #[must_use]
    pub fn new(fact_store: Arc<SqliteFactStore>) -> Self {
        let path = resolve_resource_path("config/persona_cards.json");
        Self {
            fact_store,
            cards_path: path.to_string_lossy().into_owned(),
        }
    }
}

#[async_trait]
impl ToolHandler for PersonaInjectTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let agent_id = args
            .get("agent_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("missing `agent_id`".into()))?;
        let tenant_id = args
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let format = args.get("format").and_then(Value::as_str).unwrap_or("json");

        // Resolve the agent entity and read its accumulated facts, then
        // aggregate a persona card from the `agent_personality`-tagged ones.
        let entity_id = self.fact_store.resolve_agent(tenant_id, agent_id)?;
        let facts = self.fact_store.get_facts(entity_id)?;
        let mut card = build_persona_card_from_facts(tenant_id, agent_id, &facts);

        // Merge the optional JSON persona card file entry (file wins, missing
        // fields fall back to the aggregated card).
        let cards = load_persona_cards(&self.cards_path)?;
        if let Some(entry) = lookup_persona_card(&cards, tenant_id, agent_id) {
            card = merge_persona_card_file(card, &entry);
        }

        let payload = match format {
            "text" => render_text(&card),
            _ => render_json(tenant_id, agent_id, &card),
        };
        Ok(ToolCallResult::text(payload))
    }
}

/// Render the card as a JSON object `{tenant_id, agent_id, persona_card}`.
fn render_json(tenant_id: &str, agent_id: &str, card: &PersonaCard) -> String {
    json!({
        "tenant_id": tenant_id,
        "agent_id": agent_id,
        "persona_card": card,
    })
    .to_string()
}

/// Render the card as a text block for a system prompt.
fn render_text(card: &PersonaCard) -> String {
    persona_card_to_text(card)
}

/// Return the stable MCP schema for `persona_inject`.
pub fn persona_inject_definition() -> ToolDefinition {
    ToolDefinition {
        name: "persona_inject".into(),
        description: "Inject a structured, deterministic persona card (人设) for an agent into the system prompt. Aggregates the stored agent_personality facts into identity/persona/relationship, optionally merged with a hand-authored persona card JSON. No LLM, read-only.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "agent_id": {"type": "string", "description": "Agent/entity identifier whose persona card is injected"},
                "tenant_id": {"type": "string", "default": "default", "description": "Tenant namespace for the agent entity"},
                "format": {"type": "string", "enum": ["text", "json"], "default": "json", "description": "Output format: 'text' for a system-prompt block, 'json' for a structured object"}
            },
            "required": ["agent_id"]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_personality::AGENT_PERSONALITY_ATTRIBUTION;
    use crate::cognition::{Fact, FactType};

    /// Seed a persona fact for an agent entity through the real fact store.
    fn seed_persona_fact(
        store: &SqliteFactStore,
        entity_id: i64,
        fact_type: FactType,
        content: &str,
    ) {
        store
            .insert_fact(&Fact {
                id: None,
                entity_id,
                fact_type,
                time: 1,
                payload: json!({
                    "attribution": AGENT_PERSONALITY_ATTRIBUTION,
                    "content": content,
                    "negated": false,
                }),
                evidence_id: None,
                created_at: 1,
            })
            .expect("insert persona fact");
    }

    /// Parse the tool call's text payload into a JSON value.
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

    /// Build a tool pointing at a temporary (likely missing) card file so the
    /// fact-aggregation path is exercised.
    fn tool_with_path(store: Arc<SqliteFactStore>, path: &str) -> PersonaInjectTool {
        PersonaInjectTool {
            fact_store: store,
            cards_path: path.to_string(),
        }
    }

    /// Objective: Verify the tool aggregates facts into a persona card and
    /// returns it as JSON with the expected top-level shape.
    /// Invariants: persona_card has identity, persona, relationship; top-level
    /// carries tenant_id / agent_id.
    #[tokio::test]
    async fn json_output_aggregates_fact_card() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let entity_id = store
            .resolve_agent("tenant-a", "agent-bailiusu")
            .expect("resolve agent");
        seed_persona_fact(&store, entity_id, FactType::Identity, "我是白流苏");
        seed_persona_fact(&store, entity_id, FactType::Preference, "我喜欢安稳");
        let tool = tool_with_path(store, "/nonexistent/persona_cards.json");

        let result = tool
            .call(&json!({
                "tenant_id": "tenant-a",
                "agent_id": "agent-bailiusu"
            }))
            .await
            .expect("call succeeds");
        let payload = parse_payload(&result);
        assert_eq!(payload["tenant_id"], json!("tenant-a"));
        assert_eq!(payload["agent_id"], json!("agent-bailiusu"));
        assert_eq!(payload["persona_card"]["identity"], json!("我是白流苏"));
        let persona = payload["persona_card"]["persona"]
            .as_array()
            .expect("persona");
        assert!(persona.iter().any(|v| v == &json!("我喜欢安稳")));
    }

    /// Objective: Verify `format=text` output contains the identity line.
    /// Invariants: the text payload contains "我是白流苏".
    #[tokio::test]
    async fn text_output_contains_identity() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let entity_id = store
            .resolve_agent("tenant-a", "agent-bailiusu")
            .expect("resolve agent");
        seed_persona_fact(
            &store,
            entity_id,
            FactType::Identity,
            "我是白流苏，离过婚，爱过，也输过",
        );
        let tool = tool_with_path(store, "/nonexistent/persona_cards.json");

        let result = tool
            .call(&json!({
                "tenant_id": "tenant-a",
                "agent_id": "agent-bailiusu",
                "format": "text"
            }))
            .await
            .expect("call succeeds");
        let text = result
            .content
            .first()
            .and_then(|b| b.text.clone())
            .unwrap_or_default();
        assert!(
            text.contains("我是白流苏，离过婚，爱过，也输过"),
            "text output includes identity, got: {text}"
        );
    }

    /// Objective: Verify a JSON persona card entry overrides the aggregated
    /// card, filling `style`/`taboos` (which facts never produce) while the
    /// absent-file fields fall back to the aggregated values.
    /// Invariants: file style/taboos/identity win; aggregated persona persists.
    #[tokio::test]
    async fn json_card_merges_with_fact_fallback() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let entity_id = store
            .resolve_agent("tenant-a", "agent-bailiusu")
            .expect("resolve agent");
        seed_persona_fact(&store, entity_id, FactType::Identity, "我是白流苏");
        seed_persona_fact(&store, entity_id, FactType::Preference, "我喜欢安稳");

        let dir = std::env::temp_dir();
        let path = dir.join("persona_cards_inject_test.json");
        std::fs::write(
            &path,
            r#"{
                "tenant-a": {
                    "agent-bailiusu": {
                        "identity": "白流苏，离过婚，爱过，也输过",
                        "style": ["话少，克制，偶尔揶揄"],
                        "taboos": ["绝不说自己已经放下"]
                    }
                }
            }"#,
        )
        .expect("write card file");
        let tool = tool_with_path(store, path.to_str().expect("utf8 path"));

        let result = tool
            .call(&json!({
                "tenant_id": "tenant-a",
                "agent_id": "agent-bailiusu"
            }))
            .await
            .expect("call succeeds");
        let payload = parse_payload(&result);
        let card = &payload["persona_card"];
        assert_eq!(
            card["identity"],
            json!("白流苏，离过婚，爱过，也输过"),
            "file identity wins"
        );
        assert_eq!(
            card["style"],
            json!(["话少，克制，偶尔揶揄"]),
            "file style fills in"
        );
        assert_eq!(card["taboos"], json!(["绝不说自己已经放下"]));
        let persona = card["persona"].as_array().expect("persona");
        assert!(
            persona.iter().any(|v| v == &json!("我喜欢安稳")),
            "absent file persona falls back to aggregated facts"
        );

        std::fs::remove_file(&path).ok();
    }

    /// Objective: Verify the tool rejects a call missing the required
    /// `agent_id` argument with an InvalidInput error.
    /// Invariants: calling without agent_id → Err(InvalidInput).
    #[tokio::test]
    async fn missing_agent_id_is_invalid_input() {
        let store = Arc::new(SqliteFactStore::open_in_memory().expect("fact store"));
        let tool = tool_with_path(store, "/nonexistent/persona_cards.json");
        let err = tool
            .call(&json!({}))
            .await
            .expect_err("missing agent_id must error");
        assert!(
            matches!(err, Error::InvalidInput(_)),
            "expected InvalidInput, got {err:?}"
        );
        assert!(err.to_string().contains("agent_id"));
    }

    /// Objective: Verify the tool schema requires `agent_id`.
    /// Invariants: the definition's required array contains `agent_id`.
    #[test]
    fn definition_requires_agent_id() {
        let def = persona_inject_definition();
        let required = def
            .input_schema
            .get("required")
            .and_then(Value::as_array)
            .expect("required array");
        let names: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
        assert!(names.contains(&"agent_id"), "schema requires agent_id");
        assert_eq!(def.name, "persona_inject");
    }
}
