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
use crate::compiler::CompileContext;
use crate::compiler::entity::{CorpusEntityProvider, EntityDictionary, EntityProvider};
use crate::compiler::extract;
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

/// Whether a document title looks like a person name, i.e. worth
/// materializing as the document's anchor `person` object.
///
/// Auto-generated titles (`generalize-1786331280`, session ids) and document
/// filenames ("export.json") are NOT names — materializing them as `person`
/// entities polluted the graph with fake nodes whose names were document
/// titles. Only a CJK name-shaped title (2-6 ideographs) or a single
/// Capitalized English word ("Alice", "Mr. Smith") qualifies.
fn looks_like_person_name(title: &str) -> bool {
    let t = title.trim();
    if t.is_empty() {
        return false;
    }
    // CJK name: 2-6 contiguous ideographs.
    let chars: Vec<char> = t.chars().collect();
    if chars.iter().all(|c| ('\u{4e00}'..='\u{9fff}').contains(c)) {
        return (2..=6).contains(&chars.len());
    }
    // English name: a single Capitalized word, optionally with a title
    // prefix ("Mr. Smith" → ends with a Capitalized surname).
    let words: Vec<&str> = t.split_whitespace().collect();
    if words.is_empty() || words.len() > 3 {
        return false;
    }
    let last = words[words.len() - 1];
    let mut last_chars = last.chars();
    let first = last_chars.next();
    if !matches!(first, Some(c) if c.is_ascii_uppercase()) {
        return false;
    }
    last_chars.all(|c| c.is_ascii_lowercase())
}

/// Counters for one pipeline run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PipelineStats {
    pub documents: usize,
    pub objects: usize,
    pub edges: usize,
    pub evidence: usize,
    /// Pass2 story events persisted to the V7 `events` table (each carries
    /// a source byte span).
    pub events: usize,
    /// Character-state slots persisted to `world_states` (extracted from
    /// Pass2 events — e.g. kill → object `status=deceased`).
    pub states: usize,
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
        //
        // KNOWN LIMITATION: identity is title-only. Two different sources that
        // share a title (e.g. both auto-titled "conversation") merge into one
        // doc_id and attach each other's objects. A full fix needs a `source`
        // column on `documents` plus a (title, source) unique key — tracked as
        // a schema change, not patched here with a non-existent API.
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
        //
        //    Dialog/conversation documents are the deliberate exception: their
        //    title is a session/conversation id, NOT a person, so materializing
        //    a `person` object named after the title pollutes the graph with a
        //    fake entity (observed: three empty `person` nodes whose names are
        //    dialog titles). Dialog speakers are already captured by the
        //    cognition layer (`agent_personality` / `companion_extract`) as
        //    typed facts, so the general pipeline skips the title entity for
        //    dialogs while still persisting evidence rows (they carry doc_id).
        let is_dialog = doc.doc_type.contains("dialog") || doc.doc_type.contains("conversation");
        // (attr_key, keyword_hit, sentence_text, start, end) — the span keeps
        // each claim re-locatable after the keyword is stripped from context.
        let mut attributes: Vec<(String, String, String, usize, usize)> = Vec::new();
        let mut relation_targets: Vec<String> = Vec::new();
        let mut evidence_texts: Vec<(String, usize, usize)> = Vec::new();

        for sent in &sentences {
            let t = sent.text.trim();
            if t.is_empty() {
                continue;
            }
            // Attributes: any extraction hint keyword hit → (attr_key, hit)
            // plus the full sentence span for evidence anchoring.
            for (attr, keywords) in &profile.extraction_hints {
                if let Some(kw) = keywords.iter().find(|k| t.contains(k.as_str())) {
                    attributes.push((
                        attr.clone(),
                        kw.clone(),
                        t.to_string(),
                        sent.start_offset,
                        sent.end_offset,
                    ));
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
                evidence_texts.push((t.to_string(), sent.start_offset, sent.end_offset));
            }
        }

        // Deduplicate attributes (same key may hit multiple keywords). Keep the
        // first occurrence's span so the evidence anchor is stable across runs.
        attributes.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        attributes.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
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
            "attributes": attributes
                .iter()
                .map(|(k, v, _, _, _)| serde_json::json!([k, v]))
                .collect::<Vec<_>>(),
            "relations": relations,
            "doc_type": doc.doc_type,
        });

        // ③b+③c only apply to non-dialog documents whose title is an actual
        // person name; otherwise `entity_id` stays `None` and evidence rows
        // are persisted without an entity link. Auto-generated titles
        // ("generalize-1786331280") or filenames are NOT names — creating a
        // `person` object named after them polluted the graph with fake
        // entities (observed: knowledge_objects full of document titles
        // instead of the characters inside the text).
        let entity_id: Option<i64> = if is_dialog || !looks_like_person_name(&doc.title) {
            None
        } else {
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

            // ③c Sync the entity into the V7 world model (`world_entities` +
            //     `world_entity_profiles`), so the general pipeline also lands in
            //     the entity-centric tables that were previously left empty
            //     ("V7 integration"). Attributes become key/value profiles;
            //     upsert is idempotent by entity name and by (entity_id, key).
            let world_entity_id = store.upsert_world_entity(&doc.title, "person", 0.5).await?;
            for (key, value, sent_text, sent_start, sent_end) in &attributes {
                // Anchor the claim sentence so the profile row is re-locatable.
                // Without this the keyword alone cannot be found in the source.
                let evidence_id = store
                    .create_evidence(&Evidence {
                        id: 0,
                        doc_id,
                        chapter_id,
                        start_offset: Some(*sent_start as i64),
                        end_offset: Some(*sent_end as i64),
                        content: sent_text.clone(),
                        created_at: now_ts(),
                    })
                    .await?;
                store
                    .upsert_world_profile(world_entity_id, key, value, 0.8, Some(evidence_id))
                    .await?;
                stats.evidence += 1;
            }
            Some(entity_id)
        };

        // ③d Corpus entity discovery — for non-dialog prose, detect the cast
        //     from the actual text (speakers before dialogue verbs) instead of
        //     relying on a hand-maintained dictionary. Each discovered entity
        //     becomes its own person object and a V7 world entity, so a text
        //     about 刘备/关羽/曹操 yields three entities, not just the doc
        //     title. Entities already present or equal to the doc title are
        //     skipped (no duplication).
        if !(doc.doc_type.contains("dialog") || doc.doc_type.contains("conversation")) {
            // ③d Corpus entity discovery. `min_frequency = 2` gates out
            // one-off speaker fragments ("一壶", "何故" — a phrase or a
            // narration word captured before a dialogue verb): a real cast
            // member recurs across the text, a fragment usually does not.
            let corpus = CorpusEntityProvider::from_text(&doc.title, &doc.text, 2);
            let mut cast: Vec<String> = vec![doc.title.clone()];
            for entry in corpus.entries() {
                if entry.canonical_name == doc.title {
                    continue;
                }
                if store
                    .find_object_by_name(&entry.canonical_name, Some(doc_id))
                    .await?
                    .is_some()
                {
                    cast.push(entry.canonical_name.clone());
                    continue;
                }
                store
                    .create_object(&KnowledgeObject {
                        id: 0,
                        doc_id,
                        object_type: ObjectType::Person,
                        name: entry.canonical_name.clone(),
                        properties: serde_json::json!({
                            "discovered": true,
                            "source": "corpus",
                            "frequency": entry
                                .properties
                                .get("frequency")
                                .cloned()
                                .unwrap_or_else(|| "0".to_string()),
                        }),
                        confidence: 0.7,
                        created_at: now_ts(),
                    })
                    .await?;
                store
                    .upsert_world_entity(&entry.canonical_name, "person", 0.6)
                    .await?;
                stats.objects += 1;
                cast.push(entry.canonical_name.clone());
            }
            // ③f Novel dictionary (NEW-C20): `NovelProvider` carries the
            // curated character tables (ingest::characters) that the shipped
            // JSON profiles leave with `"entities": []`, and it was never
            // instantiated in production. Register known characters that
            // actually appear in the text (canonical name OR any alias), so
            // the real cast — including characters who narrate but rarely
            // speak — lands in the graph with their aliases, instead of only
            // dialogue speakers. Unknown doc titles yield zero entries.
            let novel = crate::compiler::entity::NovelProvider::new(&doc.title);
            for entry in novel.entries() {
                if entry.canonical_name == doc.title {
                    continue;
                }
                let present = entry.aliases.iter().any(|a| doc.text.contains(a.as_str()))
                    || doc.text.contains(entry.canonical_name.as_str());
                if !present {
                    continue;
                }
                if store
                    .find_object_by_name(&entry.canonical_name, Some(doc_id))
                    .await?
                    .is_some()
                {
                    continue;
                }
                store
                    .create_object(&KnowledgeObject {
                        id: 0,
                        doc_id,
                        object_type: ObjectType::Person,
                        name: entry.canonical_name.clone(),
                        properties: serde_json::json!({
                            "source": "novel_dictionary",
                            "aliases": entry.aliases,
                            "novel": doc.title,
                        }),
                        confidence: 0.85,
                        created_at: now_ts(),
                    })
                    .await?;
                store
                    .upsert_world_entity(&entry.canonical_name, "person", 0.7)
                    .await?;
                stats.objects += 1;
                cast.push(entry.canonical_name.clone());
            }
            // ③e Story-event extraction: turn narrative sentences into Event
            //     objects + `participated_in` edges so `person_key_events` has a
            //     real trajectory to distill (never invoked for dialog).
            if !cast.is_empty() {
                let events = crate::compiler::story_events::materialize_story_events(
                    store, doc_id, chapter_id, &cast, &doc.text,
                )
                .await?;
                stats.edges += events;

                // ③f Pass2 story compile → persist world events with byte spans.
                //     The dict is the discovered cast (plus the title person),
                //     so mention resolution never invents participants. Each
                //     Event lands in `events` with its sentence/verb span, making
                //     the middle-IR offsets durable (evidence-traceable).
                let mut dict = EntityDictionary::default();
                for name in &cast {
                    dict.register_discovered(name, &[]);
                }
                if looks_like_person_name(&doc.title) {
                    dict.register_discovered(&doc.title, &[]);
                }
                let sent_spans: Vec<(&str, usize, usize)> = sentences
                    .iter()
                    .map(|s| (s.text.as_str(), s.start_offset, s.end_offset))
                    .collect();
                let mut ctx = CompileContext::default();
                extract::compile(
                    &mut ctx,
                    &sent_spans,
                    &dict,
                    &extract::Config::default(),
                    None,
                );
                for ev in &ctx.events {
                    let event_id = store
                        .upsert_world_event(
                            &ev.title,
                            &ev.event_type,
                            ev.timestamp,
                            ev.location.as_deref(),
                            &ev.description,
                            ev.importance,
                            ev.start_offset.map(|v| v as i64),
                            ev.end_offset.map(|v| v as i64),
                        )
                        .await?;
                    for p in &ev.participants {
                        store
                            .link_event_participant(event_id, &p.entity_name, &p.role)
                            .await?;
                    }
                    stats.events += 1;

                    // ③g Character-state slots: deterministic verb rules over
                    //     this event (kill → object deceased, …). Each slot
                    //     reuses the event's id/chapter/span so the state row
                    //     is as traceable as the event that produced it.
                    for slot in crate::compiler::state_slots::extract_state_slots(ev) {
                        store
                            .upsert_world_state(
                                &slot.entity,
                                &slot.slot,
                                &slot.value,
                                slot.chapter,
                                Some(event_id),
                                slot.start_offset,
                                slot.end_offset,
                                slot.confidence,
                            )
                            .await?;
                        stats.states += 1;
                    }
                }
            }
        }

        // ④ Persist evidence (original sentences, for traceability).
        for (sent_text, sent_start, sent_end) in &evidence_texts {
            let ev = Evidence {
                id: 0,
                doc_id,
                chapter_id,
                // Exact original-text span — content alone cannot re-locate a
                // repeated sentence; the vision requires byte offsets.
                start_offset: Some(*sent_start as i64),
                end_offset: Some(*sent_end as i64),
                content: sent_text.clone(),
                created_at: now_ts(),
            };
            let ev_id = store.create_evidence(&ev).await?;
            // Dialog documents have no title entity (their title is a session
            // id, not a person), so the evidence row still exists — linked to
            // the document via `doc_id` — but has no object link.
            if let Some(entity_id) = entity_id {
                store
                    .link_evidence(EvidenceSourceType::Object, entity_id, ev_id)
                    .await?;
            }
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

    /// Objective: Verify a dialog source compiles into the general model
    /// WITHOUT materializing a `person` object named after the session title
    /// (a dialog title is a session id, not a person — the fixed modeling).
    /// Invariants: stats.documents == 1; stats.objects == 0; evidence >= 1;
    /// no object named after the session title exists.
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
        assert_eq!(
            stats.objects, 0,
            "dialog title must NOT become a person entity, got {stats:?}"
        );
        assert!(stats.evidence >= 1, "hint sentences become evidence");

        // The session title must not exist as a knowledge object.
        assert!(
            store
                .find_object_by_name("session-1", None)
                .await
                .expect("query")
                .is_none(),
            "dialog title must not be materialized as an object"
        );
    }

    /// Objective: Verify a txt file compiles end-to-end (FileSource path) and
    /// extracts the person names from the body — not just the filename.
    /// Invariants: stats.documents == 1; the character 刘备 from the body is
    /// discovered (the old behavior materialized the FILE stem as a fake
    /// `person` object instead of the text's people).
    #[tokio::test]
    async fn txt_compiles_into_general_model() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let path = std::env::temp_dir().join(format!(
            "lorescope_pipeline_test_{}_{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::write(
            &path,
            "刘备说：我计划下周发布新版本。关羽答道：同意。刘备又说：那就按灰度方案来。关羽再道：好。",
        )
        .expect("write");
        let source = crate::knowledge::document_source::FileSource::new(&path);
        let stats = compile_source(&source, &profile(), &store, "t1")
            .await
            .expect("compile");
        assert_eq!(stats.documents, 1);
        assert!(
            stats.objects >= 1,
            "txt file must yield the body's people, got {stats:?}"
        );
        assert!(
            store
                .find_object_by_name("刘备", None)
                .await
                .expect("query")
                .is_some(),
            "刘备 (from the body) must be extracted as an entity"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Objective: Verify a novel-titled text registers known characters from
    /// the novel dictionary (NEW-C20) — a character who narrates but rarely
    /// speaks must still land in the graph with its aliases.
    /// Invariants: 赵云 exists with source=novel_dictionary and 子龙 among
    /// aliases; the corpus-only path would miss him (no dialogue verb).
    #[tokio::test]
    async fn novel_dictionary_registers_known_cast() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let text = "赵云字子龙，常山真定人也。其人身长八尺，姿颜雄伟。";
        let source =
            crate::knowledge::document_source::RawTextSource::new("三国演义", "test", text, "text");
        let stats = compile_source(&source, &profile(), &store, "t1")
            .await
            .expect("compile");
        assert!(stats.objects >= 1, "novel cast registered, got {stats:?}");

        let zhaoyun = store
            .find_object_by_name("赵云", None)
            .await
            .expect("query")
            .expect("赵云 must exist via novel dictionary");
        assert_eq!(
            zhaoyun.properties.get("source").and_then(|v| v.as_str()),
            Some("novel_dictionary"),
            "registered from the curated dictionary"
        );
        let aliases = zhaoyun
            .properties
            .get("aliases")
            .and_then(|a| a.as_array())
            .expect("aliases array");
        assert!(
            aliases.iter().any(|a| a.as_str() == Some("子龙")),
            "alias 子龙 must be attached, got {aliases:?}"
        );
        // The world model must also carry the entity.
        let we = store
            .find_world_entity("赵云")
            .await
            .expect("query")
            .expect("world entity");
        assert_eq!(we.name, "赵云");
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

    /// Objective: Verify entity upsert — compiling the same non-dialog text
    /// title twice reuses the existing entity instead of creating a duplicate.
    /// Invariants: first run objects == 1; second run objects == 0; exactly
    /// one persisted object named after the title (the title is a real name,
    /// so it legitimately anchors the document's entity).
    #[tokio::test]
    async fn recompile_same_title_reuses_entity() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let source = crate::knowledge::document_source::RawTextSource::new(
            "刘备",
            "export.json",
            "我喜欢 Rust，目标是稳定可靠。",
            "text",
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
            .find_object_by_name("刘备", None)
            .await
            .expect("query")
            .expect("entity still present");
        assert_eq!(obj.name, "刘备", "single entity persists");
    }

    /// Objective: Verify relation sentences do NOT spawn placeholder Concept
    /// entities ("no phantom entities" rule) and are instead recorded on the
    /// entity as structured `relations` records.
    /// Invariants: one document → objects == 1 (main entity only), edges == 0,
    /// and the entity's `relations` array is non-empty.
    #[tokio::test]
    async fn relation_sentences_do_not_spawn_concepts() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let source = crate::knowledge::document_source::RawTextSource::new(
            "曹操",
            "export.json",
            "我喜欢简洁架构，目标是长期稳定。",
            "text",
        );

        let stats = compile_source(&source, &profile(), &store, "t1")
            .await
            .expect("compile");
        assert_eq!(stats.documents, 1, "one document → one document");
        assert_eq!(
            stats.objects, 1,
            "no phantom concept entities, got {stats:?}"
        );
        assert_eq!(stats.edges, 0, "no edges to placeholder targets");

        let obj = store
            .find_object_by_name("曹操", None)
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

    /// Objective: Verify the general pipeline also lands in the V7 world model
    /// (`world_entities` + `world_entity_profiles`), which were previously
    /// left empty ("V7 integration").
    /// Invariants: after compiling a text document whose title is a real
    /// person name, a `world_entities` row exists named after it, with at
    /// least one profile when hints were extracted. Non-name titles (session
    /// ids, generated ids) are excluded by design.
    #[tokio::test]
    async fn compile_writes_v7_world_entities() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let source = crate::knowledge::document_source::RawTextSource::new(
            "刘备",
            "export.json",
            "我喜欢简洁架构，目标是长期稳定。",
            "text",
        );

        let stats = compile_source(&source, &profile(), &store, "t1")
            .await
            .expect("compile");
        assert_eq!(stats.documents, 1, "one document compiled");

        let world = store
            .find_world_entity("刘备")
            .await
            .expect("query world entity")
            .expect("world_entities row must be written by compile");
        assert_eq!(world.entity_type, "person", "default entity type");
        assert_eq!(world.name, "刘备", "entity name matches the title");

        // Every profile claim must carry an evidence row with a non-empty
        // source byte span — content-only anchors are not re-locatable.
        let profiles = store.list_world_profiles().await.expect("list profiles");
        assert!(
            !profiles.is_empty(),
            "hint extraction must produce at least one world profile"
        );
        for p in &profiles {
            let evidence_id = p
                .evidence_id
                .unwrap_or_else(|| panic!("profile {} must carry evidence_id", p.key));
            let mut found = false;
            for doc in store.list_documents().await.expect("docs") {
                for ev in store.list_evidence_by_document(doc.id).await.expect("ev") {
                    if ev.id == evidence_id {
                        found = true;
                        assert!(
                            ev.start_offset.is_some() && ev.end_offset.is_some(),
                            "profile {} evidence must carry byte offsets",
                            p.key
                        );
                        assert!(
                            ev.end_offset > ev.start_offset,
                            "profile {} evidence span must be non-empty",
                            p.key
                        );
                    }
                }
            }
            assert!(found, "profile {} evidence_id must resolve", p.key);
        }
    }

    /// Objective: Verify Pass2 story events persist to the V7 `events` table
    /// with non-empty source byte spans (the production write path for
    /// middle-IR Event offsets).
    /// Invariants: prose with a discovered cast produces >= 1 world event;
    /// every event carries start/end offsets with start < end; a second
    /// compile of the same source does not duplicate events.
    #[tokio::test]
    async fn prose_persists_world_events_with_byte_spans() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let source = crate::knowledge::document_source::RawTextSource::new(
            "刘备",
            "export.json",
            "刘备曰：今日起兵。关羽道：愿随兄长。刘备救关羽于阵前。",
            "text",
        );

        let stats = compile_source(&source, &profile(), &store, "t1")
            .await
            .expect("compile");
        assert!(
            stats.events >= 1,
            "Pass2 must persist at least one world event, got {stats:?}"
        );

        let events = store.list_world_events().await.expect("list events");
        assert!(
            !events.is_empty(),
            "events table must hold the compiled events"
        );
        for ev in &events {
            let start = ev
                .start_offset
                .unwrap_or_else(|| panic!("event `{}` must carry start_offset", ev.title));
            let end = ev
                .end_offset
                .unwrap_or_else(|| panic!("event `{}` must carry end_offset", ev.title));
            assert!(
                start < end,
                "event `{}` span must be non-empty, got {start}..{end}",
                ev.title
            );
        }

        // Re-compile: identical (title, timestamp, span) must not duplicate.
        let before = store.list_world_events().await.expect("list before").len();
        let second = compile_source(&source, &profile(), &store, "t1")
            .await
            .expect("recompile");
        let after = store.list_world_events().await.expect("list after").len();
        assert_eq!(before, after, "re-compile must not duplicate world events");
        assert!(second.events <= before, "second run reuses existing rows");
    }

    /// Objective: Verify the production write path for character-state slots
    /// — Pass2 kill events extract `status=deceased` for the victim and
    /// persist it to `world_states` with the event anchor + byte span.
    /// Invariants: prose with a novel cast yields >= 1 state row; the victim
    /// has slot=status/value=deceased; every row carries event_id and a
    /// non-empty span; a second compile does not duplicate rows.
    #[tokio::test]
    async fn prose_persists_world_state_slots() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        // Novel title → NovelProvider seeds 吕布/董卓 into the cast, so the
        // Pass2 dict can resolve both participants of the kill.
        let source = crate::knowledge::document_source::RawTextSource::new(
            "三国演义",
            "export.json",
            "吕布杀董卓。",
            "text",
        );

        let stats = compile_source(&source, &profile(), &store, "t1")
            .await
            .expect("compile");
        assert!(
            stats.states >= 1,
            "kill event must yield a state slot, got {stats:?}"
        );

        let states = store.list_world_states(None).await.expect("list");
        assert!(!states.is_empty(), "world_states must hold the slot");
        let victim = states
            .iter()
            .find(|s| s.slot == "status" && s.value == "deceased")
            .unwrap_or_else(|| panic!("a deceased status slot must exist, got {states:?}"));
        assert_eq!(
            victim.entity_name, "董卓",
            "kill marks the OBJECT, not the killer"
        );
        assert!(
            victim.event_id.is_some(),
            "state row must anchor to its source event"
        );
        let start = victim
            .start_offset
            .unwrap_or_else(|| panic!("state must carry start_offset"));
        let end = victim
            .end_offset
            .unwrap_or_else(|| panic!("state must carry end_offset"));
        assert!(start < end, "state span must be non-empty: {start}..{end}");

        // Re-compile: same (entity, slot, event, chapter) is a no-op.
        let before = store.list_world_states(None).await.expect("before").len();
        compile_source(&source, &profile(), &store, "t1")
            .await
            .expect("recompile");
        let after = store.list_world_states(None).await.expect("after").len();
        assert_eq!(before, after, "re-compile must not duplicate state rows");
    }

    /// Objective: Verify dialog titles do NOT leak into the V7 world model —
    /// a session id must not become a world entity (same modeling fix as the
    /// object graph).
    /// Invariants: compiling a dialog yields no `world_entities` row named
    /// after the session title.
    #[tokio::test]
    async fn dialog_title_not_in_v7_world_entities() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let source = DialogSource::new(
            "session-no-v7",
            "export.json",
            vec![Message::new("user", "我喜欢 Rust，目标是稳定可靠。")],
        );

        let stats = compile_source(&source, &profile(), &store, "t1")
            .await
            .expect("compile");
        assert_eq!(stats.objects, 0, "dialog → no objects");

        let world = store
            .find_world_entity("session-no-v7")
            .await
            .expect("query");
        assert!(
            world.is_none(),
            "dialog title must not become a world entity"
        );
    }

    /// Objective: Verify corpus entity discovery — a non-dialog text with
    /// several speakers yields entities for the main speakers, not just the
    /// doc-title anchor. This is the "entities come from the text, not a
    /// dictionary" guarantee.
    ///
    /// The heuristic is intentionally tolerant: prose like `大哥所言` may also
    /// surface an honorary term, but the *data* always comes from the text,
    /// and the main speakers are always present. We assert the core cast is
    /// discovered (anchor + 刘备 + 关羽), not an exact object count.
    /// Invariants: the three core entities exist and corpus ones are flagged;
    /// every discovered corpus entity recurs >= 2 times (frequency gate).
    #[tokio::test]
    async fn corpus_text_discovers_multiple_entities() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let source = crate::knowledge::document_source::RawTextSource::new(
            "会谈纪要",
            "paste",
            "刘备说道：此事需从长计议。关羽道：大哥所言极是。刘备又说道：那便依计行事。关羽曰：此计可行。",
            "text",
        );

        let stats = compile_source(&source, &profile(), &store, "t1")
            .await
            .expect("compile");
        // Anchor + the discovered cast: must exceed the single-anchor case.
        assert!(
            stats.objects >= 3,
            "corpus discovery must surface the speakers, got {stats:?}"
        );

        for name in ["会谈纪要", "刘备", "关羽"] {
            let obj = store
                .find_object_by_name(name, None)
                .await
                .expect("query")
                .unwrap_or_else(|| panic!("entity `{name}` must be discovered"));
            if name != "会谈纪要" {
                assert_eq!(
                    obj.properties["discovered"], true,
                    "corpus entities are flagged as discovered"
                );
                let freq = obj.properties["frequency"]
                    .as_str()
                    .unwrap_or_default()
                    .parse::<usize>()
                    .unwrap_or(0);
                assert!(
                    freq >= 2,
                    "corpus entity `{name}` must recur >=2 times, got freq={freq}"
                );
            }
        }
    }
}
