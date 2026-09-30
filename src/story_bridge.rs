//! Novel → persona-fact bridge (acceptance item 8, novel side).
//!
//! The persona-evolution timeline ([`crate::persona::timeline`]) reads from the
//! **fact store**, but novel prose is compiled by `generalize_compile` into the
//! **knowledge graph** (`Object` / `Edge`). Those two stores are disjoint, so a
//! novel protagonist's trajectory is invisible to `persona_timeline` on its own.
//!
//! This module bridges the gap: it walks a protagonist's `participated_in`
//! story events in the knowledge graph, re-expresses each event's text as a
//! persona signal via the existing, battle-tested
//! [`crate::agent_personality::agent_personality_facts_from_messages`] extractor,
//! and writes the resulting `Fact`s (plus the raw events) onto a fact-store
//! entity under `tenant_id` / `character_name`. After a bridge, calling
//! `build_timeline_for_entity(&fact_store, entity_id)` yields a real
//! `起点 → 转折点 → 现状` arc for a novel character — matching the timeline the
//! companion-dialogue path already produces for live conversation.

use std::collections::HashSet;

use crate::agent_personality::agent_personality_facts_from_messages;
use crate::cognition::{Fact, FactStore as _, FactType};
use crate::error::Result;
use crate::fact_store::SqliteFactStore;
use crate::knowledge::SQLiteKnowledgeStore;
use crate::knowledge::store::KnowledgeStore;
use crate::types::Message;

/// The stride between consecutive bridged facts' logical `time`. Deliberately
/// large (>= the timeline's `LARGE_GAP_THRESHOLD`) so distinct story beats show
/// up as turning points rather than one contiguous blur.
const TIME_STRIDE: i64 = 2_000_000;

/// Returns `true` when `fact` was written by the story bridge itself.
///
/// Bridge rows are the only ones the bridge may count or re-write; a fact the
/// dialogue path happened to store on the same entity must never be reported as
/// bridge output (09-26/H15).
fn is_bridge_fact(fact: &Fact) -> bool {
    fact.payload.get("source").and_then(|v| v.as_str()) == Some("story_bridge")
}

/// Outcome of a bridge run.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BridgeStats {
    /// The fact-store entity the protagonist's persona facts were attached to.
    pub entity_id: Option<i64>,
    /// Number of persona facts (Identity / Emotion / Preference / Goal)
    /// extracted from the character's story events.
    pub persona_facts: usize,
    /// Number of raw Event facts (the protagonist's experience beats).
    pub event_facts: usize,
    /// Number of distinct story events found in the knowledge graph.
    pub story_events: usize,
}

/// Bridge a protagonist's knowledge-graph story events into fact-store persona
/// facts, returning the target entity id so a caller can immediately build the
/// evolution timeline.
///
/// # Errors
///
/// Returns an error when the character cannot be resolved in the knowledge
/// graph or when either store fails.
pub async fn bridge_story_events_to_persona(
    kstore: &SQLiteKnowledgeStore,
    fstore: &SqliteFactStore,
    tenant_id: &str,
    character_name: &str,
) -> Result<BridgeStats> {
    let mut stats = BridgeStats::default();

    // 1. Resolve the character object in the knowledge graph.
    // `find_object_by_alias` already resolves exact names AND unambiguous
    // substring aliases (e.g. querying "白流苏" finds the stored "流苏").
    let obj = kstore.find_object_by_alias(character_name, None).await?;
    let Some(obj) = obj else {
        return Ok(stats);
    };

    // 2. Collect the character's participated_in events, sorted by creation
    //    order (approximating the novel's chronological flow).
    //
    // Endpoint direction guard: an inverted edge has the person as
    // `target_id`; taking `e.target_id` blindly would put the character's own
    // name into `event_ids` and write an Event fact about themselves
    // (inspect_entity_query already guards this case).
    let edges = kstore.get_edges_touching(obj.id).await?;
    let mut event_ids: Vec<i64> = edges
        .iter()
        .filter(|e| e.predicate == "participated_in")
        .map(|e| {
            if e.source_id == obj.id {
                e.target_id
            } else {
                e.source_id
            }
        })
        .filter(|id| *id != obj.id)
        .collect();
    event_ids.sort_unstable();
    event_ids.dedup();
    stats.story_events = event_ids.len();

    // 3. Re-express each event's text as persona signals + a raw Event fact.
    let entity_id = fstore.resolve_agent(tenant_id, character_name)?;
    stats.entity_id = Some(entity_id);

    // Idempotent but RETRYABLE re-run: collect the events already bridged and
    // write only the missing ones. The previous guard skipped whenever *any*
    // story_bridge fact existed, so a run that died part-way left half the
    // beats permanently missing — a retry was silently a no-op while the
    // returned stats still looked like a success (09-26/H15).
    //
    // New rows carry the knowledge-graph event id in their payload; rows from
    // before that marker only carry the event text, so both are consulted to
    // keep legacy stores from being re-bridged (and doubled).
    let existing = fstore.get_facts(entity_id)?;
    let bridged_ids: HashSet<i64> = existing
        .iter()
        .filter(|f| is_bridge_fact(f))
        .filter_map(|f| {
            f.payload
                .get("event_id")
                .and_then(serde_json::Value::as_i64)
        })
        .collect();
    let bridged_texts: HashSet<String> = existing
        .iter()
        .filter(|f| f.fact_type == FactType::Event && is_bridge_fact(f))
        .filter_map(|f| f.payload.get("content").and_then(|v| v.as_str()))
        .map(str::to_string)
        .collect();

    let mut pending: Vec<Fact> = Vec::new();
    for (index, event_id) in event_ids.iter().enumerate() {
        // Logical time is derived from the event's position, not from how many
        // events this particular run happens to write, so a retry writes the
        // missing beats at exactly the time a full first run would have used.
        // `saturating_mul` keeps very long novels (past i64::MAX / TIME_STRIDE
        // events) monotonic instead of wrapping negative.
        let time = (index as i64).saturating_mul(TIME_STRIDE);
        let Some(event) = kstore.get_object(*event_id).await? else {
            continue;
        };
        let text = event.name.clone();
        if text.is_empty() {
            continue;
        }
        if bridged_ids.contains(event_id) || bridged_texts.contains(&text) {
            continue;
        }

        // Persona signal from the event sentence (assistant-speaker voice).
        let msg = Message::new("assistant", &text);
        let persona = agent_personality_facts_from_messages(&[msg], entity_id, time);
        for mut f in persona {
            f.payload["source"] = serde_json::Value::String("story_bridge".into());
            f.payload["event_id"] = serde_json::json!(event_id);
            pending.push(f);
        }

        // Raw Event fact so the trajectory also carries the experience beats.
        pending.push(Fact {
            id: None,
            entity_id,
            fact_type: FactType::Event,
            time,
            payload: serde_json::json!({
                "content": text,
                "source": "story_bridge",
                "character": character_name,
                "event_id": event_id,
            }),
            evidence_id: None,
            created_at: time,
            ..Fact::default()
        });
    }

    // One transaction for the whole batch: a partial write can no longer leave
    // the entity half-bridged, and if the write fails nothing is committed, so
    // the next run retries cleanly.
    fstore.insert_batch(&pending)?;

    // Report what is actually in the store rather than what was attempted. The
    // old stats counted every fact on the entity (including rows written by the
    // dialogue path) and were computed before the guard, so a skipped re-run
    // still returned a "successful" number (09-26/H15).
    let bridged = fstore.get_facts(entity_id)?;
    stats.persona_facts = bridged
        .iter()
        .filter(|f| is_bridge_fact(f) && f.fact_type != FactType::Event)
        .count();
    stats.event_facts = bridged
        .iter()
        .filter(|f| is_bridge_fact(f) && f.fact_type == FactType::Event)
        .count();

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cognition::StateEngine;
    use crate::compiler::pipeline::compile_source;
    use crate::knowledge::document_source::RawTextSource;
    use crate::knowledge::domain_profile::conversation_profile;
    use crate::persona::timeline::{MilestoneType, build_evolution_timeline};

    /// Objective: verify a novel protagonist's story events become a usable
    /// persona timeline (起点 → 转折 → 现状), acceptance item 8 novel side.
    /// Invariants: events + persona facts land on one fact-store entity; the
    /// timeline has a start, a current, and at least one milestone.
    #[tokio::test]
    async fn novel_events_bridge_into_evolution_timeline() {
        let kstore = SQLiteKnowledgeStore::open_in_memory()
            .await
            .expect("kstore");
        // Compile a tiny 倾城之恋-style prose so 流苏 has participated_in events.
        let source = RawTextSource::new(
            "倾城之恋",
            "corpus",
            "流苏说道：我一个人惯了。范柳原笑道：你何苦这样。流苏又说：我宁可一个人走夜路。流苏最后道：你走你的，我不再回头。",
            "text",
        );
        let profile = conversation_profile();
        let _stats = compile_source(&source, profile, &kstore)
            .await
            .expect("compile");

        let fstore = SqliteFactStore::open_in_memory().expect("fstore");
        let bridged = bridge_story_events_to_persona(&kstore, &fstore, "default", "流苏")
            .await
            .expect("bridge");
        assert!(bridged.story_events >= 1, "protagonist has story events");
        let entity_id = bridged.entity_id.expect("bridged entity");

        let facts = fstore.get_facts(entity_id).expect("facts");
        assert!(!facts.is_empty(), "bridge must write facts");

        let timeline = build_evolution_timeline(&facts);
        assert!(timeline.start.is_some(), "timeline has an origin");
        assert!(timeline.current.is_some(), "timeline has a present");
        assert!(
            timeline.milestones.iter().any(|m| {
                matches!(
                    m.milestone_type,
                    MilestoneType::StanceFlip | MilestoneType::NewTheme
                )
            }),
            "timeline exposes a turning point (stance flip or new theme)"
        );
        let _ = StateEngine::new();
    }

    /// Objective: verify the bridge is a no-op for an unknown character.
    /// Invariants: stats stay empty, no entity is created, no error.
    #[tokio::test]
    async fn bridge_unknown_character_is_noop() {
        let kstore = SQLiteKnowledgeStore::open_in_memory()
            .await
            .expect("kstore");
        let fstore = SqliteFactStore::open_in_memory().expect("fstore");
        let bridged = bridge_story_events_to_persona(&kstore, &fstore, "default", "不存在的人")
            .await
            .expect("noop bridge");
        assert_eq!(bridged.story_events, 0);
        assert_eq!(bridged.persona_facts, 0);
        assert_eq!(bridged.event_facts, 0);
    }

    /// Objective: verify the logical-time accumulation never overflows for a
    /// protagonist with more story events than fit in i64::MAX / TIME_STRIDE
    /// (~1073). A plain `time += TIME_STRIDE` would panic in debug builds and
    /// wrap negative in release; saturating add must keep times monotonic
    /// non-decreasing instead.
    /// Invariants: all events bridge successfully; fact times never decrease.
    #[tokio::test]
    async fn bridge_many_events_never_overflows() {
        let kstore = SQLiteKnowledgeStore::open_in_memory()
            .await
            .expect("kstore");
        let doc_id = kstore
            .create_document(&crate::knowledge::Document {
                id: 0,
                title: "长篇小说".into(),
                author: None,
                doc_type: Some("novel".into()),
                source: String::new(),
                created_at: 1,
            })
            .await
            .expect("create doc");
        let protagonist = kstore
            .create_object(&crate::knowledge::KnowledgeObject {
                id: 0,
                doc_id,
                object_type: crate::knowledge::ObjectType::Person,
                name: "主角".into(),
                properties: serde_json::json!({}),
                confidence: 1.0,
                created_at: 1,
            })
            .await
            .expect("create protagonist");

        // 1100 participated_in events — past the ~1073 overflow point.
        let n_events: usize = 1100;
        for i in 0..n_events {
            let event_id = kstore
                .create_object(&crate::knowledge::KnowledgeObject {
                    id: 0,
                    doc_id,
                    object_type: crate::knowledge::ObjectType::Event,
                    name: format!("第{i}回 主角行动"),
                    properties: serde_json::json!({}),
                    confidence: 1.0,
                    created_at: 1,
                })
                .await
                .expect("create event");
            kstore
                .create_edge(&crate::knowledge::KnowledgeEdge {
                    id: 0,
                    source_id: protagonist,
                    target_id: event_id,
                    predicate: "participated_in".into(),
                    properties: serde_json::json!({}),
                    origin: crate::knowledge::Origin::Observed,
                    confidence: 1.0,
                    valid_from: None,
                    valid_to: None,
                    created_at: 1,
                })
                .await
                .expect("create edge");
        }

        let fstore = SqliteFactStore::open_in_memory().expect("fstore");
        let bridged = bridge_story_events_to_persona(&kstore, &fstore, "default", "主角")
            .await
            .expect("bridge many events");
        assert_eq!(
            bridged.story_events, n_events,
            "all story events must be discovered"
        );
        assert_eq!(
            bridged.event_facts, n_events,
            "every story event must produce a raw Event fact"
        );

        let entity_id = bridged.entity_id.expect("bridged entity");
        let facts = fstore.get_facts(entity_id).expect("facts");
        let times: Vec<i64> = facts
            .iter()
            .filter(|f| f.fact_type == crate::cognition::FactType::Event)
            .map(|f| f.time)
            .collect();
        assert_eq!(times.len(), n_events, "one time per event fact");
        for pair in times.windows(2) {
            assert!(
                pair[1] >= pair[0],
                "fact times must be monotonic non-decreasing, got {} then {}",
                pair[0],
                pair[1]
            );
        }
    }

    /// Build a knowledge store holding a protagonist with `n` `participated_in`
    /// story events named `第{i}回 主角行动`, returning the store and the ids.
    async fn story_fixture(n: usize) -> (SQLiteKnowledgeStore, Vec<i64>) {
        let kstore = SQLiteKnowledgeStore::open_in_memory()
            .await
            .expect("kstore");
        let doc_id = kstore
            .create_document(&crate::knowledge::Document {
                id: 0,
                title: "测试小说".into(),
                author: None,
                doc_type: Some("novel".into()),
                source: String::new(),
                created_at: 1,
            })
            .await
            .expect("create doc");
        let protagonist = kstore
            .create_object(&crate::knowledge::KnowledgeObject {
                id: 0,
                doc_id,
                object_type: crate::knowledge::ObjectType::Person,
                name: "主角".into(),
                properties: serde_json::json!({}),
                confidence: 1.0,
                created_at: 1,
            })
            .await
            .expect("create protagonist");

        let mut event_ids = Vec::with_capacity(n);
        for i in 0..n {
            let event_id = kstore
                .create_object(&crate::knowledge::KnowledgeObject {
                    id: 0,
                    doc_id,
                    object_type: crate::knowledge::ObjectType::Event,
                    name: format!("第{i}回 主角行动"),
                    properties: serde_json::json!({}),
                    confidence: 1.0,
                    created_at: 1,
                })
                .await
                .expect("create event");
            kstore
                .create_edge(&crate::knowledge::KnowledgeEdge {
                    id: 0,
                    source_id: protagonist,
                    target_id: event_id,
                    predicate: "participated_in".into(),
                    properties: serde_json::json!({}),
                    origin: crate::knowledge::Origin::Observed,
                    confidence: 1.0,
                    valid_from: None,
                    valid_to: None,
                    created_at: 1,
                })
                .await
                .expect("create edge");
            event_ids.push(event_id);
        }
        (kstore, event_ids)
    }

    /// Objective: Verify a partially-written bridge stays retryable. A run that
    /// died after the first event used to be skipped forever by the
    /// "any story_bridge fact exists" guard (09-26/H15).
    /// Invariants: The retry bridges the two missing events, every event ends
    /// with exactly one bridge Event fact (no duplicates), and the returned
    /// event count matches the store.
    #[tokio::test]
    async fn partial_bridge_is_retryable() {
        let (kstore, event_ids) = story_fixture(3).await;
        let fstore = SqliteFactStore::open_in_memory().expect("fstore");
        let entity = fstore.resolve_agent("default", "主角").expect("entity");

        // Simulate a run that died right after bridging event 0.
        fstore
            .insert_fact(&Fact {
                id: None,
                entity_id: entity,
                fact_type: FactType::Event,
                time: 0,
                payload: serde_json::json!({
                    "content": "第0回 主角行动",
                    "source": "story_bridge",
                    "event_id": event_ids[0],
                }),
                evidence_id: None,
                created_at: 0,
                ..Fact::default()
            })
            .expect("seed partial bridge");

        let bridged = bridge_story_events_to_persona(&kstore, &fstore, "default", "主角")
            .await
            .expect("retry bridge");
        assert_eq!(bridged.story_events, 3, "all three events are discovered");
        assert_eq!(
            bridged.event_facts, 3,
            "the retry must complete the two missing events"
        );

        let facts = fstore.get_facts(entity).expect("facts");
        let event_texts: Vec<&str> = facts
            .iter()
            .filter(|f| f.fact_type == FactType::Event && is_bridge_fact(f))
            .filter_map(|f| f.payload.get("content").and_then(|v| v.as_str()))
            .collect();
        assert_eq!(
            event_texts.len(),
            3,
            "each event must have exactly one bridge Event fact, got {event_texts:?}"
        );
        for event_id in &event_ids {
            assert!(
                facts
                    .iter()
                    .any(|f| f.payload.get("event_id").and_then(|v| v.as_i64()) == Some(*event_id)),
                "event {event_id} must be marked as bridged"
            );
        }
    }

    /// Objective: Verify a completed bridge re-run reports the true store
    /// contents (bridge-sourced facts only) and never duplicates rows, instead
    /// of returning a "successful" number from a skipped guard (09-26/H15).
    /// Invariants: The second run's stats equal the first's; a non-bridge fact
    /// on the same entity is present but excluded from the reported counts.
    #[tokio::test]
    async fn bridge_rerun_reports_accurate_stats() {
        let (kstore, _events) = story_fixture(2).await;
        let fstore = SqliteFactStore::open_in_memory().expect("fstore");
        let entity = fstore.resolve_agent("default", "主角").expect("entity");
        // A fact the dialogue path would have written on the same entity: it
        // must never be reported as bridge output.
        fstore
            .insert_fact(&Fact {
                id: None,
                entity_id: entity,
                fact_type: FactType::Identity,
                time: 0,
                payload: serde_json::json!({ "content": "主角是对话产物" }),
                evidence_id: None,
                created_at: 0,
                ..Fact::default()
            })
            .expect("seed non-bridge fact");

        let first = bridge_story_events_to_persona(&kstore, &fstore, "default", "主角")
            .await
            .expect("first bridge");
        assert_eq!(
            first.event_facts, 2,
            "one bridge Event fact per story event"
        );

        let second = bridge_story_events_to_persona(&kstore, &fstore, "default", "主角")
            .await
            .expect("second bridge");
        assert_eq!(
            second.event_facts, first.event_facts,
            "an idempotent re-run must not grow the event count"
        );
        assert_eq!(
            second.persona_facts, first.persona_facts,
            "an idempotent re-run must not grow the persona count"
        );

        let facts = fstore.get_facts(entity).expect("facts");
        let total_persona = facts
            .iter()
            .filter(|f| f.fact_type != FactType::Event)
            .count();
        assert!(
            total_persona > second.persona_facts,
            "the non-bridge fact must exist but be excluded from persona_facts"
        );
    }
}
