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
use crate::knowledge::store::{KnowledgeStore, NewWorldEvent, NewWorldState};
use crate::knowledge::{EvidenceSourceType, KnowledgeObject, ObjectType};

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
        // ① Persist the document: identity is (title, source). Two different
        // sources sharing a title (e.g. both auto-titled "conversation")
        // used to merge into one doc_id and attach each other's objects —
        // the provenance tag on `documents` now keeps them apart. Legacy
        // rows (source='') stay reachable through `find_document_by_title`
        // for title-only read paths.
        let doc_id = match store.find_document(&doc.title, &doc.source).await? {
            Some(existing) => existing.id,
            None => {
                store
                    .create_document(&crate::knowledge::Document {
                        id: 0,
                        title: doc.title.clone(),
                        author: doc.author.clone(),
                        doc_type: Some(doc.doc_type.clone()),
                        source: doc.source.clone(),
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
                // ensure_evidence reuses an identical (span, content) row from a
                // previous compile instead of multiplying duplicates.
                let (evidence_id, created) = store
                    .ensure_evidence(
                        doc_id,
                        chapter_id,
                        Some(*sent_start as i64),
                        Some(*sent_end as i64),
                        sent_text,
                    )
                    .await?;
                if created {
                    stats.evidence += 1;
                }
                store
                    .upsert_world_profile(world_entity_id, key, value, 0.8, Some(evidence_id))
                    .await?;
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
                // bilingual() = zh defaults + en verb tables: the pipeline
                // accepts any caller text, and `Config::default()` alone is
                // Chinese-only (English prose yielded zero action events).
                extract::compile(
                    &mut ctx,
                    &sent_spans,
                    &dict,
                    &extract::Config::bilingual(),
                    None,
                );
                for ev in &ctx.events {
                    let event_id = store
                        .upsert_world_event(NewWorldEvent {
                            title: &ev.title,
                            event_type: &ev.event_type,
                            timestamp: ev.timestamp,
                            location: ev.location.as_deref(),
                            description: &ev.description,
                            importance: ev.importance,
                            start_offset: ev.start_offset.map(|v| v as i64),
                            end_offset: ev.end_offset.map(|v| v as i64),
                        })
                        .await?;
                    for p in &ev.participants {
                        store
                            .link_event_participant(event_id, &p.entity_name, &p.role)
                            .await?;
                    }
                    stats.events += 1;

                    // ③g Character-state slots: deterministic verb rules over
                    //     this event (kill → object deceased, …), with the
                    //     source-context guard so embedded verbs (死守/杀出)
                    //     cannot flip status. Each slot reuses the event's
                    //     id/chapter/span so the state row is as traceable as
                    //     the event that produced it.
                    for slot in crate::compiler::state_slots::extract_state_slots(ev, &doc.text) {
                        store
                            .upsert_world_state(NewWorldState {
                                entity_name: &slot.entity,
                                slot: &slot.slot,
                                value: &slot.value,
                                chapter: slot.chapter,
                                event_id: Some(event_id),
                                start_offset: slot.start_offset,
                                end_offset: slot.end_offset,
                                confidence: slot.confidence,
                            })
                            .await?;
                        stats.states += 1;
                    }
                }
            }
        }

        // ④ Persist evidence (original sentences, for traceability).
        //    ensure_evidence makes re-compiles idempotent: identical
        //    (span, content) rows are reused instead of duplicated, and the
        //    idempotent link below re-attaches the entity to the same row.
        for (sent_text, sent_start, sent_end) in &evidence_texts {
            let (ev_id, created) = store
                .ensure_evidence(
                    doc_id,
                    chapter_id,
                    Some(*sent_start as i64),
                    Some(*sent_end as i64),
                    sent_text,
                )
                .await?;
            if created {
                stats.evidence += 1;
            }
            // Dialog documents have no title entity (their title is a session
            // id, not a person), so the evidence row still exists — linked to
            // the document via `doc_id` — but has no object link.
            if let Some(entity_id) = entity_id {
                store
                    .link_evidence(EvidenceSourceType::Object, entity_id, ev_id)
                    .await?;
            }
        }
    }

    Ok(stats)
}
