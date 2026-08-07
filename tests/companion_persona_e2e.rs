//! End-to-end test for the companion-AI persona tools (阶段E2).
//!
//! Verifies the full closed loop over an in-memory SQLite fact store:
//!
//!   persona_inject (inject a persona card)
//!     → persona_check (guard a draft reply against the stored persona)
//!     → relationship_update (roll relationship state from messages)
//!     → relationship_query (read the state back)
//!     → persona_timeline (rebuild the evolution timeline)
//!
//! A `NullEmbedder` is used so `persona_check` deterministically exercises the
//! offline keyword fallback path (no embedding backend required).

use std::sync::Arc;

use lore_scope::agent_personality::AGENT_PERSONALITY_ATTRIBUTION;
use lore_scope::cognition::{Fact, FactStore, FactType};
use lore_scope::embed::NullEmbedder;
use lore_scope::fact_store::SqliteFactStore;
use lore_scope::mcp::decay_tool::MemoryDecayTool;
use lore_scope::mcp::persona_check_tool::PersonaCheckTool;
use lore_scope::mcp::persona_inject_tool::PersonaInjectTool;
use lore_scope::mcp::relationship_tool::{
    PersonaTimelineTool, RelationshipQueryTool, RelationshipUpdateTool,
};
use lore_scope::mcp::types::{ToolCallResult, ToolHandler};
use serde_json::{Value, json};

const TENANT_ID: &str = "default";
const AGENT_ID: &str = "agent-bailiusu";
const USER_ID: &str = "alice";

/// Build a persona fact tagged `agent_personality`; `entity_id` is filled in
/// after the agent entity is resolved.
fn persona_fact(fact_type: FactType, time: i32, negated: bool, content: &str) -> Fact {
    Fact {
        id: None,
        entity_id: 0,
        fact_type,
        time,
        payload: json!({
            "attribution": AGENT_PERSONALITY_ATTRIBUTION,
            "content": content,
            "negated": negated,
        }),
        evidence_id: None,
        created_at: i64::from(time),
    }
}

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

#[tokio::test]
async fn companion_persona_full_loop() {
    let store = Arc::new(SqliteFactStore::open_in_memory().expect("open fact store"));

    // 1. Seed the agent's persona facts. The second and third facts form a
    //    genuine stance flip (喜欢应酬 → 不喜欢应酬) so the timeline later
    //    reports a turning point, and `persona_check` can flag a contradiction.
    //    Fact 4 (喜欢安稳) is a single-stance topic used by stage 3b to verify
    //    a genuine reversal with no same-direction anchor.
    let entity_id = store
        .resolve_agent(TENANT_ID, AGENT_ID)
        .expect("resolve agent");
    let mut facts = vec![
        persona_fact(FactType::Identity, 1, false, "我是白流苏，离过婚"),
        persona_fact(FactType::Preference, 2, false, "我喜欢应酬"),
        persona_fact(FactType::Preference, 3, true, "我不喜欢应酬"),
        persona_fact(FactType::Preference, 4, false, "我喜欢安稳"),
    ];
    for fact in &mut facts {
        fact.entity_id = entity_id;
    }
    let inserted = store.insert_batch(&facts).expect("insert persona facts");
    assert_eq!(inserted, 4, "all four persona facts are persisted");

    // 2. persona_inject: aggregate a structured persona card from the facts.
    let inject = PersonaInjectTool::new(store.clone());
    let inject_result = inject
        .call(&json!({
            "agent_id": AGENT_ID,
            "tenant_id": TENANT_ID,
            "format": "json",
        }))
        .await
        .expect("persona_inject succeeds");
    let inject_payload = parse_payload(&inject_result);
    let card = &inject_payload["persona_card"];
    assert!(
        card["identity"]
            .as_str()
            .unwrap_or("")
            .contains("我是白流苏"),
        "injected card carries the identity, got: {card}"
    );
    assert_eq!(inject_payload["agent_id"], json!(AGENT_ID));

    // 3. persona_check: verify the draft matching the CURRENT stance is NOT
    //    falsely flagged. Facts 2 and 3 form a stance flip (我喜欢应酬 →
    //    我不喜欢应酬); the draft "我讨厌应酬" aligns with the current negated
    //    stance, so after the same-direction-priority fix (persona/check.rs)
    //    it is consistent rather than a conflict against the stale affirmative.
    let check = PersonaCheckTool::new(store.clone(), Arc::new(NullEmbedder)).await;
    let check_result = check
        .call(&json!({
            "agent_id": AGENT_ID,
            "tenant_id": TENANT_ID,
            "draft": "我讨厌应酬，太累了。",
        }))
        .await
        .expect("persona_check succeeds");
    let check_payload = parse_payload(&check_result);
    assert_eq!(
        check_payload["clean"],
        json!(true),
        "draft aligned with the current stance must be clean, not a stale conflict: {check_payload}"
    );
    let conflicts = check_payload["conflicts"]
        .as_array()
        .expect("conflicts array");
    assert!(
        conflicts.is_empty(),
        "no conflict when the draft matches the current stance"
    );

    // 3b. A draft that genuinely contradicts the CURRENT stance (opposite
    //     negation, no same-direction anchor) must still be flagged. Fact 4
    //     ("我喜欢安稳", negated=false) is a single-stance topic — "我讨厌安稳"
    //     reverses it with no affirmative anchor to shield it.
    let contra_result = check
        .call(&json!({
            "agent_id": AGENT_ID,
            "tenant_id": TENANT_ID,
            "draft": "我讨厌安稳，太吵了。",
        }))
        .await
        .expect("persona_check succeeds");
    let contra_payload = parse_payload(&contra_result);
    assert_eq!(
        contra_payload["clean"],
        json!(false),
        "a genuine reversal of a single-stance topic must be flagged: {contra_payload}"
    );
    let contra_conflicts = contra_payload["conflicts"]
        .as_array()
        .expect("conflicts array");
    assert!(
        !contra_conflicts.is_empty(),
        "a genuine contradiction is reported"
    );
    assert_eq!(
        contra_conflicts[0]["fact_type"],
        json!("Preference"),
        "flagged conflict is a Preference reversal"
    );

    // 4. relationship_update: roll intimacy from positive/negative emotion
    //    user messages (deterministic rules, no LLM).
    let update = RelationshipUpdateTool::new(store.clone());
    let update_result = update
        .call(&json!({
            "tenant_id": TENANT_ID,
            "agent_id": AGENT_ID,
            "user_id": USER_ID,
            "messages": [
                {"role": "user", "content": "谢谢你，今天很开心！"},
                {"role": "user", "content": "工作压力很大，有点累。"},
                {"role": "user", "content": "谢谢你的温暖陪伴。"},
                {"role": "assistant", "content": "我一直在。"},
            ],
        }))
        .await
        .expect("relationship_update succeeds");
    let update_payload = parse_payload(&update_result);
    assert_eq!(update_payload["exists"], json!(true));
    let intimacy = update_payload["intimacy"]
        .as_f64()
        .expect("intimacy is a number");
    assert!(
        intimacy > 0.0,
        "intimacy moved up from the positive messages"
    );
    assert!(
        update_payload["stage"].as_str().is_some(),
        "stage is present"
    );

    // 5. relationship_query: read the persisted snapshot back.
    let query = RelationshipQueryTool::new(store.clone());
    let query_result = query
        .call(&json!({
            "tenant_id": TENANT_ID,
            "agent_id": AGENT_ID,
            "user_id": USER_ID,
        }))
        .await
        .expect("relationship_query succeeds");
    let query_payload = parse_payload(&query_result);
    assert_eq!(query_payload["exists"], json!(true));
    assert_eq!(
        query_payload["intimacy"], update_payload["intimacy"],
        "query returns the persisted intimacy"
    );

    // 6. persona_timeline: rebuild the ADD-only evolution timeline.
    let timeline = PersonaTimelineTool::new(store.clone());
    let timeline_result = timeline
        .call(&json!({ "entity_id": entity_id }))
        .await
        .expect("persona_timeline succeeds");
    let timeline_payload = parse_payload(&timeline_result);
    assert!(
        timeline_payload["start"].is_object(),
        "timeline has a start"
    );
    assert!(
        timeline_payload["current"].is_object(),
        "timeline has a current"
    );
    let trajectory = timeline_payload["trajectory"]
        .as_array()
        .expect("trajectory array");
    assert_eq!(trajectory.len(), 4, "ADD-only trajectory keeps all facts");
    assert!(
        timeline_payload["milestones"]
            .as_array()
            .expect("milestones array")
            .iter()
            .any(|m| m["type"] == "stance_flip"),
        "the stance flip (喜欢→不喜欢) is a turning point"
    );

    // 7. memory_decay: scan statistics without deleting any fact.
    let decay = MemoryDecayTool::new(store.clone());
    let decay_result = decay
        .call(&json!({ "entity_id": entity_id }))
        .await
        .expect("memory_decay succeeds");
    let decay_payload = parse_payload(&decay_result);
    assert_eq!(decay_payload["scanned"], json!(4), "all four facts scanned");
    assert_eq!(
        decay_payload["high_value_protected"],
        json!(4),
        "persona facts are protected from decay"
    );
}
