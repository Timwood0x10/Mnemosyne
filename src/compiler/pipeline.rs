//! Unified pipeline orchestration — the generalization plan's compile()
//! entry point.
//!
//! Wires the three building blocks into one deterministic, zero-LLM path:
//!
//! ```text
//! DocumentSource (file / dialog / …) ──load──▶ Vec<ExternalDoc>
//!       │
//!       ├─ split: sentence segmentation (reuses compiler::sentence)
//!       ├─ extract: domain Profile-pack rules (hints / predicates / verbs)
//!       └─ persist: V7 general model (objects / edges / evidence)
//! ```
//!
//! Every extracted fact carries original-text evidence and an `Observed`
//! origin — "facts come from compilation, not guesses". Consumers pass any
//! [`DocumentSource`] plus the matching [`DomainProfile`] and get rows in the
//! general knowledge model (backed by `WORLD_SCHEMA`/`KNOWLEDGE_SCHEMA`).

use serde::Serialize;

use crate::compiler::Chunk;
use crate::compiler::sentence;
use crate::error::Result;
use crate::knowledge::document_source::DocumentSource;
use crate::knowledge::domain_profile::DomainProfile;
use crate::knowledge::store::KnowledgeStore;
use crate::knowledge::{Evidence, EvidenceSourceType, KnowledgeObject, ObjectType};

/// Current unix timestamp (created_at convention across the knowledge store).
fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Counters for one pipeline run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PipelineStats {
    pub documents: usize,
    pub objects: usize,
    pub edges: usize,
    pub evidence: usize,
}

/// Run the unified pipeline: load → split → extract → persist.
///
/// # Errors
///
/// - Source load failures propagate ([`crate::error::Error::Io`] /
///   [`crate::error::Error::InvalidInput`] from the source).
/// - Store persistence failures propagate.
pub async fn compile_source(
    source: &dyn DocumentSource,
    profile: &DomainProfile,
    store: &dyn KnowledgeStore,
    _tenant_id: &str,
) -> Result<PipelineStats> {
    let docs = source.load()?;
    let mut stats = PipelineStats::default();

    for doc in docs {
        stats.documents += 1;
        // ① Persist the document (idempotent title check → reuse or create).
        let doc_id = match store.find_document_by_title(&doc.title).await? {
            Some(existing) => existing.id,
            None => {
                store
                    .create_document(&crate::knowledge::Document {
                        id: 0,
                        title: doc.title.clone(),
                        author: doc.author.clone(),
                        doc_type: Some(doc.doc_type.clone()),
                        created_at: now_ts(),
                    })
                    .await?
            }
        };

        // ①b Ensure a chapter exists: evidence.chapter_id has a FK to
        //     chapters(id), so rows must reference a real chapter — using 0
        //     violates the constraint (observed during pipeline testing).
        let chapter_id = match store.get_chapter_by_no(doc_id, 1).await? {
            Some(ch) => ch.id,
            None => {
                store
                    .create_chapter(&crate::knowledge::Chapter {
                        id: 0,
                        doc_id,
                        chapter_no: 1,
                        title: Some(doc.title.clone()),
                        content: String::new(),
                        start_offset: None,
                        end_offset: None,
                    })
                    .await?
            }
        };

        // ② Split into sentences.
        let chunk = Chunk {
            index: 0,
            text: doc.text.clone(),
            start_offset: 0,
            end_offset: doc.text.len(),
            segment_num: 0,
            overlap_before: 0,
            overlap_after: 0,
        };
        let sentences = sentence::split_chunk(&chunk);

        // ③ Extract + persist one entity per document (title = entity name),
        //    with profile attributes collected from hint keywords.
        let mut attributes: Vec<(String, String)> = Vec::new();
        let mut relation_targets: Vec<String> = Vec::new();
        let mut evidence_texts: Vec<String> = Vec::new();

        for sent in &sentences {
            let t = sent.text.trim();
            if t.is_empty() {
                continue;
            }
            // Attributes: any extraction hint keyword hit → (attr_key, hit).
            for (attr, keywords) in &profile.extraction_hints {
                if let Some(kw) = keywords.iter().find(|k| t.contains(k.as_str())) {
                    attributes.push((attr.clone(), kw.clone()));
                }
            }
            // Relations: any relation predicate hit → collect the sentence as
            // a relation candidate.
            if profile
                .relation_predicates
                .iter()
                .any(|p| t.contains(p.as_str()))
            {
                relation_targets.push(t.to_string());
            }
            // Evidence: sentences with any verb-group verb are worth keeping.
            if profile
                .verb_groups
                .values()
                .flatten()
                .any(|v| t.contains(v.as_str()))
            {
                evidence_texts.push(t.to_string());
            }
        }

        // Deduplicate attributes (same key may hit multiple keywords).
        attributes.sort();
        attributes.dedup();
        // Relation candidates are kept as structured records on the entity
        // (predicate + target snippet + original sentence) instead of
        // materialising a Concept node per sentence. This avoids polluting the
        // graph with meaningless `{title}-{i}` placeholder entities — the
        // "no phantom entities" rule — while still retaining the fact and its
        // provenance as evidence.
        let relations = relation_targets
            .iter()
            .map(|target| {
                let predicate = profile
                    .relation_predicates
                    .iter()
                    .find(|p| target.contains(p.as_str()))
                    .cloned()
                    .unwrap_or_else(|| "relates".to_string());
                let snippet = target
                    .split_once(predicate.as_str())
                    .map(|(_, rest)| rest.trim().chars().take(20).collect::<String>())
                    .unwrap_or_default();
                serde_json::json!({
                    "predicate": predicate,
                    "target_snippet": snippet,
                    "sentence": target,
                })
            })
            .collect::<Vec<_>>();
        let properties = serde_json::json!({
            "attributes": attributes,
            "relations": relations,
            "doc_type": doc.doc_type,
        });

        // ③b Entity upsert: reuse an existing object with the same title in
        //     this document instead of creating a duplicate on every compile.
        //     New attributes are merged in, so re-compiling the same source
        //     enriches rather than duplicates the entity.
        let entity_id = match store.find_object_by_name(&doc.title, Some(doc_id)).await? {
            Some(existing) => {
                store
                    .update_object_properties(existing.id, &properties, Some(0.9))
                    .await?;
                existing.id
            }
            None => {
                let obj = KnowledgeObject {
                    id: 0,
                    doc_id,
                    object_type: ObjectType::Person,
                    name: doc.title.clone(),
                    properties,
                    confidence: 0.8,
                    created_at: now_ts(),
                };
                let id = store.create_object(&obj).await?;
                stats.objects += 1;
                id
            }
        };

        // ④ Persist evidence (original sentences, for traceability).
        for text in &evidence_texts {
            let ev = Evidence {
                id: 0,
                doc_id,
                chapter_id,
                start_offset: None,
                end_offset: None,
                content: text.clone(),
                created_at: now_ts(),
            };
            let ev_id = store.create_evidence(&ev).await?;
            store
                .link_evidence(EvidenceSourceType::Object, entity_id, ev_id)
                .await?;
            stats.evidence += 1;
        }
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::document_source::DialogSource;
    use crate::knowledge::store::SQLiteKnowledgeStore;
    use crate::types::Message;

    fn profile() -> DomainProfile {
        DomainProfile::load("conversation_cognition").expect("pack")
    }

    /// Objective: Verify a dialog source compiles end-to-end into the general
    /// model: document + entity + attributes + evidence rows.
    /// Invariants: stats.documents == 1; stats.objects >= 1; evidence >= 1;
    /// the entity carries the extracted preference attribute.
    #[tokio::test]
    async fn dialog_compiles_into_general_model() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let source = DialogSource::new(
            "session-1",
            "export.json",
            vec![
                Message::new("user", "我喜欢 Rust，目标是把编译器做稳定。"),
                Message::new("assistant", "好的，我们一步一步来。"),
            ],
        );
        let stats = compile_source(&source, &profile(), &store, "t1")
            .await
            .expect("compile");
        assert_eq!(stats.documents, 1, "one dialog → one document");
        assert!(
            stats.objects >= 1,
            "at least the main entity, got {stats:?}"
        );
        assert!(stats.evidence >= 1, "hint sentences become evidence");

        // The entity must exist and carry the preference attribute.
        let obj = store
            .find_object_by_name("session-1", None)
            .await
            .expect("query")
            .expect("entity");
        let attrs = obj
            .properties
            .get("attributes")
            .and_then(|a| a.as_array())
            .expect("attributes array");
        assert!(
            attrs
                .iter()
                .any(|a| a[0].as_str() == Some("preference_keywords")),
            "preference hint must be captured, got {attrs:?}"
        );
    }

    /// Objective: Verify a txt file compiles end-to-end (FileSource path).
    /// Invariants: stats.documents == 1; objects >= 1.
    #[tokio::test]
    async fn txt_compiles_into_general_model() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let path = std::env::temp_dir().join("lorescope_pipeline_test.txt");
        std::fs::write(&path, "我计划下周发布新版本，决定用灰度方案。").expect("write");
        let source = crate::knowledge::document_source::FileSource::new(&path);
        let stats = compile_source(&source, &profile(), &store, "t1")
            .await
            .expect("compile");
        assert_eq!(stats.documents, 1);
        assert!(
            stats.objects >= 1,
            "txt file must yield an entity, got {stats:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Objective: Verify an empty source produces zero rows (no phantom
    /// entities).
    /// Invariants: empty dialog → stats all zero; no panic.
    #[tokio::test]
    async fn empty_source_yields_zero_stats() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let source = DialogSource::new("empty", "export.json", Vec::new());
        let stats = compile_source(&source, &profile(), &store, "t1")
            .await
            .expect("compile");
        assert_eq!(stats.documents, 0);
        assert_eq!(stats.objects, 0);
        assert_eq!(stats.evidence, 0);
    }

    /// Objective: Verify entity upsert — compiling the same document title
    /// twice reuses the existing entity instead of creating a duplicate.
    /// Invariants: first run objects == 1; second run objects == 0; exactly
    /// one persisted object named after the title.
    #[tokio::test]
    async fn recompile_same_title_reuses_entity() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let source = DialogSource::new(
            "session-reuse",
            "export.json",
            vec![Message::new("user", "我喜欢 Rust，目标是稳定可靠。")],
        );

        let first = compile_source(&source, &profile(), &store, "t1")
            .await
            .expect("first compile");
        assert_eq!(first.objects, 1, "first run creates the entity");

        let second = compile_source(&source, &profile(), &store, "t1")
            .await
            .expect("second compile");
        assert_eq!(second.objects, 0, "second run reuses, does not duplicate");

        let obj = store
            .find_object_by_name("session-reuse", None)
            .await
            .expect("query")
            .expect("entity still present");
        assert_eq!(obj.name, "session-reuse", "single entity persists");
    }

    /// Objective: Verify relation sentences do NOT spawn placeholder Concept
    /// entities ("no phantom entities" rule) and are instead recorded on the
    /// entity as structured `relations` records.
    /// Invariants: one document → objects == 1 (main entity only), edges == 0,
    /// and the entity's `relations` array is non-empty.
    #[tokio::test]
    async fn relation_sentences_do_not_spawn_concepts() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let source = DialogSource::new(
            "session-rels",
            "export.json",
            vec![Message::new("user", "我喜欢简洁架构，目标是长期稳定。")],
        );

        let stats = compile_source(&source, &profile(), &store, "t1")
            .await
            .expect("compile");
        assert_eq!(stats.documents, 1, "one dialog → one document");
        assert_eq!(
            stats.objects, 1,
            "no phantom concept entities, got {stats:?}"
        );
        assert_eq!(stats.edges, 0, "no edges to placeholder targets");

        let obj = store
            .find_object_by_name("session-rels", None)
            .await
            .expect("query")
            .expect("entity");
        let rels = obj
            .properties
            .get("relations")
            .and_then(|r| r.as_array())
            .expect("relations array present");
        assert!(!rels.is_empty(), "relation sentences recorded on entity");
    }
}
