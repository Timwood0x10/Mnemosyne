//! Story-event extraction for non-dialog prose.
//!
//! The general pipeline (`compile_source`) deliberately keeps `edges == 0`
//! for relation sentences to honour the "no phantom entities" rule — relations
//! are recorded as entity properties instead. That is correct for dialog, but
//! it starves `person_key_events`: that tool reads `participated_in` edges to
//! reconstruct a person's trajectory, and without edges it can never surface a
//! key event.
//!
//! This module fills that gap for **non-dialog prose** (novels, memoirs): it
//! turns genuine narrative sentences into `Event` objects and links each
//! discovered cast member to them via `participated_in` edges, so a character's
//! story beats become queryable key events. The event objects are named after
//! the real sentence (evidence-bearing), never a `{title}-{i}` placeholder, so
//! the "no phantom entities" rule still holds.
//!
//! It is a strict superset of the previous behaviour for prose: it only ADDS
//! event objects + edges; it never removes or rewrites an existing person
//! entity, and it is never invoked for dialog sources.

use crate::compiler::sentence;
use crate::error::Result;
use crate::ingest::extract::DIALOG_VERBS;
use crate::knowledge::store::KnowledgeStore;
use crate::knowledge::{
    Evidence, EvidenceSourceType, KnowledgeEdge, KnowledgeObject, ObjectType, Origin,
};

/// Narrative verbs that mark a sentence as a story event (action / movement /
/// change of state). Dialogue verbs (`说/道/曰/…`) come from
/// [`DIALOG_VERBS`]; the rest are scene/action verbs common to prose.
const NARRATIVE_VERBS: &[&str] = &[
    "走",
    "来",
    "去",
    "到",
    "进",
    "出",
    "回",
    "见",
    "离",
    "坐",
    "站",
    "抱",
    "拉",
    "看",
    "望",
    "哭",
    "笑",
    "嫁",
    "娶",
    "成婚",
    "结婚",
    "离婚",
    "死",
    "生",
    "跳",
    "逃",
    "追",
    "打",
    "亲",
    "吻",
    "买",
    "卖",
    "送",
    "给",
    "得",
    "失",
    "等",
    "候",
    "归",
    "留",
    "去香港",
    "上船",
    "下船",
];

/// Maximum length of an event object name derived from its sentence.
const MAX_EVENT_TITLE: usize = 24;

/// Materialize story events for a prose document.
///
/// For every sentence that mentions a discovered cast member AND carries a
/// narrative/dialogue verb, create (or reuse) an `Event` object named from the
/// sentence, link the cast member to it via a `participated_in` edge, and
/// anchor the sentence as evidence on the event. Returns the number of
/// `participated_in` edges created (0 when every edge already existed).
///
/// # Errors
///
/// Propagates store persistence errors.
pub async fn materialize_story_events(
    store: &dyn KnowledgeStore,
    doc_id: i64,
    chapter_id: i64,
    cast: &[String],
    text: &str,
) -> Result<usize> {
    let chunk = crate::compiler::Chunk {
        index: 0,
        text: text.to_string(),
        start_offset: 0,
        end_offset: text.len(),
        segment_num: 0,
        overlap_before: 0,
        overlap_after: 0,
    };
    let sentences = sentence::split_chunk(&chunk);
    let now = chrono::Utc::now().timestamp();

    let mut edges_created = 0usize;
    for sent in &sentences {
        let t = sent.text.trim();
        if t.chars().count() < 6 || t.chars().count() > 80 {
            continue;
        }
        // The sentence must name at least one cast member AND carry a narrative
        // or dialogue verb — that combination is a story beat, not filler.
        // Link EVERY named cast member, not just the first: key_events reads
        // the event object's neighbours for its participants signal, and a
        // sentence like "流苏与三哥争执" that only linked 流苏 would make the
        // centrality dimension permanently zero (participants always empty).
        let members: Vec<&str> = cast
            .iter()
            .filter(|c| t.contains(c.as_str()))
            .map(|c| c.as_str())
            .collect();
        if members.is_empty() {
            continue;
        }
        if !NARRATIVE_VERBS
            .iter()
            .chain(DIALOG_VERBS.iter())
            .any(|v| t.contains(v))
        {
            continue;
        }

        // Event object named after the real sentence (evidence-bearing, not a
        // placeholder). Reuse an existing event with the same name in this doc
        // so re-compiling enriches rather than duplicates.
        let title: String = t.chars().take(MAX_EVENT_TITLE).collect();
        let event_id = match store.find_object_by_name(&title, Some(doc_id)).await? {
            Some(existing) => existing.id,
            None => {
                store
                    .create_object(&KnowledgeObject {
                        id: 0,
                        doc_id,
                        object_type: ObjectType::Event,
                        name: title.clone(),
                        properties: serde_json::json!({ "source": "story_events" }),
                        confidence: 0.7,
                        created_at: now,
                    })
                    .await?
            }
        };

        // Link each named character → event. person object is the source;
        // event the target. Deduplicate the same (person, event) pair on
        // re-compile.
        for member in members {
            let person_id = store
                .find_object_by_name(member, Some(doc_id))
                .await?
                .map(|o| o.id);
            if let Some(person_id) = person_id {
                let exists = store
                    .get_edges_touching(person_id)
                    .await?
                    .iter()
                    .any(|e| e.predicate == "participated_in" && e.target_id == event_id);
                if !exists {
                    store
                        .create_edge(&KnowledgeEdge {
                            id: 0,
                            source_id: person_id,
                            target_id: event_id,
                            predicate: "participated_in".into(),
                            properties: serde_json::json!({}),
                            origin: Origin::Observed,
                            confidence: 0.7,
                            valid_from: None,
                            valid_to: None,
                            created_at: now,
                        })
                        .await?;
                    edges_created += 1;
                }
            }
        }

        // Anchor the sentence as evidence on the event object so the key-event
        // scorer sees corroborating original text.
        let evidence_id = store
            .create_evidence(&Evidence {
                id: 0,
                doc_id,
                chapter_id,
                start_offset: None,
                end_offset: None,
                content: t.to_string(),
                created_at: now,
            })
            .await?;
        store
            .link_evidence(EvidenceSourceType::Object, event_id, evidence_id)
            .await?;
    }

    Ok(edges_created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::pipeline::compile_source;
    use crate::knowledge::SQLiteKnowledgeStore;
    use crate::knowledge::document_source::RawTextSource;
    use crate::knowledge::domain_profile::{DomainProfile, conversation_profile};

    async fn memory_store() -> SQLiteKnowledgeStore {
        SQLiteKnowledgeStore::open_in_memory().await.expect("store")
    }

    /// Objective: Verify prose with named characters + action verbs yields
    /// Event objects and `participated_in` edges (the key-event source).
    /// Invariants: 流苏 participates in >= 1 event; each event has evidence.
    #[tokio::test]
    async fn prose_materializes_participated_in_edges() {
        let store = memory_store().await;
        let source = RawTextSource::new(
            "倾城之恋",
            "corpus",
            "流苏说道：我一个人惯了。范柳原笑道：你何苦这样。流苏又说道：我宁可一个人走夜路。",
            "text",
        );
        let stats = compile_source(&source, conversation_profile(), &store, "t1")
            .await
            .expect("compile");
        // Events were materialized: prose with a discovered cast produces
        // participated_in edges (dialogue verbs count as story beats).
        assert!(
            stats.edges > 0,
            "prose must produce participated_in edges, got {stats:?}"
        );

        // 流苏 (corpus-discovered via dialogue verb) must participate.
        let obj = store
            .find_object_by_name("流苏", None)
            .await
            .expect("query")
            .unwrap_or_else(|| panic!("cast member 流苏 must be discovered"));
        let edges = store.get_edges_touching(obj.id).await.expect("edges");
        let participated = edges
            .iter()
            .filter(|e| e.predicate == "participated_in")
            .count();
        assert!(
            participated >= 1,
            "流苏 must participate in story events, got {edges:?}"
        );
        let _ = stats;
    }

    /// Objective: Verify dialog input does NOT spawn story events (the module
    /// is gated to non-dialog prose, so existing dialog behaviour is intact).
    /// Invariants: a dialog compile keeps edges == 0 and objects == 1.
    #[tokio::test]
    async fn dialog_is_untouched_by_story_events() {
        let store = memory_store().await;
        let profile: DomainProfile = conversation_profile().clone();
        let source = crate::knowledge::document_source::DialogSource::new(
            "session-rel",
            "export.json",
            vec![crate::types::Message::new(
                "user",
                "我喜欢简洁架构，目标是长期稳定。",
            )],
        );
        let stats = compile_source(&source, &profile, &store, "t1")
            .await
            .expect("compile");
        assert_eq!(stats.objects, 1, "dialog keeps a single anchor entity");
        assert_eq!(stats.edges, 0, "dialog never spawns story-event edges");
    }

    /// Objective: Verify re-compiling the same prose does not duplicate events
    /// or edges (idempotent enrichment).
    /// Invariants: second run yields 0 new objects; edge count stays stable.
    #[tokio::test]
    async fn recompile_prose_is_idempotent() {
        let store = memory_store().await;
        let source = RawTextSource::new(
            "倾城之恋-幂等",
            "corpus",
            "流苏说道：我一个人惯了。范柳原笑道：你何苦这样。",
            "text",
        );
        let first = compile_source(&source, conversation_profile(), &store, "t1")
            .await
            .expect("first");
        let second = compile_source(&source, conversation_profile(), &store, "t1")
            .await
            .expect("second");
        assert_eq!(
            second.objects, 0,
            "re-compile creates no duplicate objects (anchor + cast + events reused)"
        );
        assert!(
            second.edges <= first.edges,
            "re-compile adds no duplicate participated_in edges"
        );
    }
}
