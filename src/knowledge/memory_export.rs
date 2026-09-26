//! Memory migration — export/import the knowledge graph as a portable JSON
//! snapshot.
//!
//! The knowledge graph is the durable carrier of an AI persona: entities,
//! their attributes, relations, and the evidence that grounds them. `export`
//! serializes the whole graph to a single JSON [`ExportBundle`]; `import`
//! replays it into any (possibly empty) store, **deduplicating by identity**
//! (document title, entity name, evidence content) so re-importing is a
//! no-op rather than a duplicate.
//!
//! This is what makes "memory is never lost" concrete: the memory can be
//! backed up, moved to another machine, or shared with a collaborator, and
//! restored byte-for-byte into the graph.
//!
//! ## Scope — what a bundle does and does not carry
//!
//! Exported: `documents` metadata (including the `source` provenance tag),
//! `objects`, `edges`, `evidence` with byte spans, object→evidence links,
//! and the V7 world model — `world_entities`, `world_profiles`,
//! `world_relations`, `world_events` (with participants), `world_states`.
//!
//! Not exported, by design:
//! - `chapters` bodies — they are rebuildable from the original sources
//!   (`compile_source` re-splits any document; `Migrator::migrate` re-reads
//!   the corpus files), so a restore re-derives them instead of shipping
//!   duplicate prose.
//! - edge→evidence links — `compile_source` only produces object→evidence
//!   links today; nothing would populate the other direction.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
pub use crate::knowledge::memory_export_world::{
    ExportEventIdentity, ExportEventParticipant, ExportWorldEvent, ExportWorldState,
};
use crate::knowledge::store::KnowledgeStore;
use crate::knowledge::{
    Document, Evidence, EvidenceSourceType, KnowledgeEdge, KnowledgeObject, ObjectType, Origin,
};

/// Format tag embedded in every bundle, for validation on import.
pub const EXPORT_FORMAT: &str = "lorescope-memory";
/// Current snapshot format version (bump on any schema-affecting change).
///
/// v2 added the `world_events`/`world_states` sections (T9); v1 bundles
/// still import (`#[serde(default)]` → empty sections).
pub const EXPORT_VERSION: u32 = 2;

/// A document row, keyed for dedup by `title`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportDocument {
    pub title: String,
    pub author: Option<String>,
    pub doc_type: Option<String>,
    /// Provenance tag — part of the write-path identity with `title`.
    /// `#[serde(default)]` so v1 bundles (no source) import as `""`.
    #[serde(default)]
    pub source: String,
}

/// A knowledge object, keyed for dedup by `(doc_title, name)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportObject {
    pub doc_title: String,
    pub object_type: String,
    pub name: String,
    pub properties: serde_json::Value,
    pub confidence: f64,
}

/// A directed relation edge, keyed for dedup by
/// `(doc_title, source_name, predicate, target_name)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportEdge {
    pub doc_title: String,
    pub source_name: String,
    pub predicate: String,
    pub target_name: String,
    pub properties: serde_json::Value,
    pub origin: String,
    pub confidence: f64,
    pub valid_from: Option<i32>,
    pub valid_to: Option<i32>,
}

/// An evidence snippet, keyed for dedup by `(doc_title, content)`.
///
/// Carries the original-text span so a restored graph stays
/// evidence-traceable: without `start_offset`/`end_offset` an import rewrote
/// every anchor to `NULL` and "backup → another machine" lost the ability to
/// locate the claim in the source document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportEvidence {
    pub doc_title: String,
    pub content: String,
    /// Byte start of the snippet in the source document, when known.
    #[serde(default)]
    pub start_offset: Option<i64>,
    /// Byte end (exclusive) of the snippet in the source document.
    #[serde(default)]
    pub end_offset: Option<i64>,
}

/// A fact↔evidence link (object evidence), keyed for dedup by
/// `(source_name, evidence_content)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportEvidenceLink {
    pub source_name: String,
    pub evidence_content: String,
}

/// A V7 world entity, keyed for dedup by `name`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportWorldEntity {
    pub name: String,
    pub entity_type: String,
    pub importance: f64,
}

/// A V7 world profile, keyed for dedup by `(entity_name, key)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportWorldProfile {
    pub entity_name: String,
    pub key: String,
    pub value: String,
    pub confidence: f64,
    /// Evidence content that anchors this claim's source span (content-keyed
    /// so the import can re-locate the row after a restore).
    #[serde(default)]
    pub evidence_content: Option<String>,
    /// Source byte span of that evidence, so a restore can prefer the row at
    /// the SAME offset when the same sentence text appears more than once.
    /// `None` on bundles exported before this field existed — import then
    /// falls back to content-only matching.
    #[serde(default)]
    pub evidence_start: Option<i64>,
    #[serde(default)]
    pub evidence_end: Option<i64>,
}

/// A V7 world relation, keyed for dedup by
/// `(source_name, relation_type, target_name)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportWorldRelation {
    pub source_name: String,
    pub relation_type: String,
    pub target_name: String,
    pub confidence: f64,
}

/// The full portable snapshot of a knowledge graph.
///
/// The `world_*` sections carry the V7 entity-centric model (compiled by the
/// general pipeline). They are `#[serde(default)]` so older bundles without
/// them still import cleanly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportBundle {
    pub format: String,
    pub version: u32,
    pub exported_at: i64,
    pub documents: Vec<ExportDocument>,
    pub objects: Vec<ExportObject>,
    pub edges: Vec<ExportEdge>,
    pub evidence: Vec<ExportEvidence>,
    pub evidence_links: Vec<ExportEvidenceLink>,
    #[serde(default)]
    pub world_entities: Vec<ExportWorldEntity>,
    #[serde(default)]
    pub world_profiles: Vec<ExportWorldProfile>,
    #[serde(default)]
    pub world_relations: Vec<ExportWorldRelation>,
    /// Narrative events + participants (T6 write path) — v2.
    #[serde(default)]
    pub world_events: Vec<ExportWorldEvent>,
    /// Character-state slot observations (T7 write path) — v2.
    #[serde(default)]
    pub world_states: Vec<ExportWorldState>,
}

/// Counts reported by [`import_bundle`], used by the MCP tool to echo what
/// was created vs reused.
#[derive(Debug, Clone, Serialize)]
pub struct ImportStats {
    pub documents_created: usize,
    pub objects_created: usize,
    pub objects_merged: usize,
    pub evidence_created: usize,
    pub edges_created: usize,
    pub links_created: usize,
}

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

/// `(start_offset, end_offset, content)` — the span-aware identity of an
/// evidence row, used as the import dedup key so the same sentence text at
/// two offsets stays two rows.
type EvidenceKey = (Option<i64>, Option<i64>, String);
/// Per-document set of already-present/created evidence identities.
type EvidenceCache = std::collections::HashMap<i64, std::collections::HashSet<EvidenceKey>>;
/// evidence id → (content, start, end) for profile anchor export.
type EvidenceAnchorMap = std::collections::HashMap<i64, (String, Option<i64>, Option<i64>)>;

/// Serialize a whole store's knowledge graph into a portable [`ExportBundle`].
///
/// # Errors
///
/// Delegates to the store's read errors.
pub async fn export_store(store: &dyn KnowledgeStore) -> Result<ExportBundle> {
    let mut documents = Vec::new();
    let mut objects = Vec::new();
    let mut edges = Vec::new();
    let mut evidence = Vec::new();
    // evidence id → (content, span), so world profiles can carry a portable
    // content+span anchor alongside their numeric evidence_id.
    let mut evidence_content_by_id: EvidenceAnchorMap = std::collections::HashMap::new();

    for doc in store.list_documents().await? {
        documents.push(ExportDocument {
            title: doc.title.clone(),
            author: doc.author.clone(),
            doc_type: doc.doc_type.clone(),
            source: doc.source.clone(),
        });

        for obj in store.list_objects_by_document(doc.id).await? {
            objects.push(ExportObject {
                doc_title: doc.title.clone(),
                object_type: obj.object_type.as_str().to_string(),
                name: obj.name.clone(),
                properties: obj.properties.clone(),
                confidence: obj.confidence,
            });
        }

        for edge in store.list_edges_by_document(doc.id).await? {
            let (source_name, target_name) = match resolve_edge_endpoint_names(store, &edge).await?
            {
                Some((s, t)) => (s, t),
                None => continue, // endpoint gone; skip a dangling edge
            };
            edges.push(ExportEdge {
                doc_title: doc.title.clone(),
                source_name,
                predicate: edge.predicate.clone(),
                target_name,
                properties: edge.properties.clone(),
                origin: edge.origin.as_str().to_string(),
                confidence: edge.confidence,
                valid_from: edge.valid_from,
                valid_to: edge.valid_to,
            });
        }

        for ev in store.list_evidence_by_document(doc.id).await? {
            evidence_content_by_id
                .insert(ev.id, (ev.content.clone(), ev.start_offset, ev.end_offset));
            evidence.push(ExportEvidence {
                doc_title: doc.title.clone(),
                content: ev.content.clone(),
                start_offset: ev.start_offset,
                end_offset: ev.end_offset,
            });
        }
    }

    // Object→evidence links (the only kind `compile_source` produces; edge
    // links are out of scope for now — see module docs).
    let mut evidence_links = Vec::new();
    for link in store.list_evidence_links().await? {
        if link.source_type != EvidenceSourceType::Object {
            continue;
        }
        let Some(obj) = store.get_object(link.source_id).await? else {
            continue;
        };
        let Some(ev) = store
            .get_evidence_for(EvidenceSourceType::Object, obj.id)
            .await?
            .into_iter()
            .find(|e| e.id == link.evidence_id)
        else {
            continue;
        };
        evidence_links.push(ExportEvidenceLink {
            source_name: obj.name.clone(),
            evidence_content: ev.content.clone(),
        });
    }

    // V7 world model: entities/profiles/relations (the general pipeline's
    // entity-centric output). Profiles and relations reference entity ids, so
    // map id → name for a portable, name-keyed representation.
    let world_entities = store.list_world_entities().await?;
    let name_by_id: std::collections::HashMap<i64, String> = world_entities
        .iter()
        .map(|e| (e.id, e.name.clone()))
        .collect();
    let world_profiles = store
        .list_world_profiles()
        .await?
        .into_iter()
        .filter_map(|p| {
            name_by_id.get(&p.entity_id).map(|name| {
                let anchor = p.evidence_id.and_then(|id| evidence_content_by_id.get(&id));
                ExportWorldProfile {
                    entity_name: name.clone(),
                    key: p.key,
                    value: p.value,
                    confidence: p.confidence,
                    // Carry content + span so import can re-create an
                    // anchor at the same offset (repeated sentences otherwise
                    // re-attach to the wrong row).
                    evidence_content: anchor.map(|(c, _, _)| c.clone()),
                    evidence_start: anchor.and_then(|(_, s, _)| *s),
                    evidence_end: anchor.and_then(|(_, _, e)| *e),
                }
            })
        })
        .collect();
    let world_relations = store
        .list_world_relations()
        .await?
        .into_iter()
        .filter_map(|r| {
            Some(ExportWorldRelation {
                source_name: name_by_id.get(&r.source_id)?.clone(),
                relation_type: r.relation_type,
                target_name: name_by_id.get(&r.target_id)?.clone(),
                confidence: r.confidence,
            })
        })
        .collect();
    let (world_events, world_states) =
        crate::knowledge::memory_export_world::export_world_sections(store).await?;

    Ok(ExportBundle {
        format: EXPORT_FORMAT.to_string(),
        version: EXPORT_VERSION,
        exported_at: now_ts(),
        documents,
        objects,
        edges,
        evidence,
        evidence_links,
        world_entities: world_entities
            .into_iter()
            .map(|e| ExportWorldEntity {
                name: e.name,
                entity_type: e.entity_type,
                importance: e.importance,
            })
            .collect(),
        world_profiles,
        world_relations,
        world_events,
        world_states,
    })
}

/// Resolve the object names at both ends of an edge, querying the store.
///
/// Returns `None` when either endpoint object no longer exists (dangling
/// edge) so the export can skip it instead of writing a broken edge.
async fn resolve_edge_endpoint_names(
    store: &dyn KnowledgeStore,
    edge: &KnowledgeEdge,
) -> Result<Option<(String, String)>> {
    let Some(source) = store.get_object(edge.source_id).await? else {
        return Ok(None);
    };
    let Some(target) = store.get_object(edge.target_id).await? else {
        return Ok(None);
    };
    Ok(Some((source.name, target.name)))
}

/// Import a bundle into a store, deduplicating by identity so a re-import
/// is a no-op. Rejects bundles whose format tag does not match ours or
/// whose version is newer than this build understands.
pub async fn import_bundle(
    store: &dyn KnowledgeStore,
    bundle: &ExportBundle,
) -> Result<ImportStats> {
    if bundle.format != EXPORT_FORMAT {
        return Err(Error::InvalidInput(format!(
            "unsupported export format `{}` (expected `{EXPORT_FORMAT}`)",
            bundle.format
        )));
    }
    if bundle.version > EXPORT_VERSION {
        return Err(Error::InvalidInput(format!(
            "export bundle version {} is newer than the supported version {EXPORT_VERSION}",
            bundle.version
        )));
    }
    let mut stats = ImportStats {
        documents_created: 0,
        objects_created: 0,
        objects_merged: 0,
        evidence_created: 0,
        edges_created: 0,
        links_created: 0,
    };
    let mut docs_by_title: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    for export_doc in &bundle.documents {
        // Identity is (title, source): two same-titled documents from
        // different sources stay separate rows on import.
        let doc_id = match store
            .find_document(&export_doc.title, &export_doc.source)
            .await?
        {
            Some(existing) => existing.id,
            None => {
                let id = store
                    .create_document(&Document {
                        id: 0,
                        title: export_doc.title.clone(),
                        author: export_doc.author.clone(),
                        doc_type: export_doc.doc_type.clone(),
                        source: export_doc.source.clone(),
                        created_at: now_ts(),
                    })
                    .await?;
                stats.documents_created += 1;
                // Guarantee a chapter so evidence FK constraints hold.
                ensure_chapter(store, id).await?;
                id
            }
        };
        // Sub-bundles (objects/edges/evidence) carry only `doc_title`, so
        // the association key stays the title; first document wins when a
        // bundle contains same-titled rows (see module docs).
        docs_by_title
            .entry(export_doc.title.clone())
            .or_insert(doc_id);
    }

    // Objects: reuse by (doc_title, name), merging properties.
    let mut obj_id_by_key: std::collections::HashMap<(String, String), i64> =
        std::collections::HashMap::new();
    for export_obj in &bundle.objects {
        let Some(&doc_id) = docs_by_title.get(&export_obj.doc_title) else {
            continue; // orphan object with no document in the bundle → skip
        };
        let object_type =
            ObjectType::from_str(&export_obj.object_type).unwrap_or(ObjectType::Concept);
        let key = (export_obj.doc_title.clone(), export_obj.name.clone());
        let obj_id = match store
            .find_object_by_name(&export_obj.name, Some(doc_id))
            .await?
        {
            Some(existing) => {
                store
                    .update_object_properties(
                        existing.id,
                        &export_obj.properties,
                        Some(export_obj.confidence),
                    )
                    .await?;
                stats.objects_merged += 1;
                existing.id
            }
            None => {
                let id = store
                    .create_object(&KnowledgeObject {
                        id: 0,
                        doc_id,
                        object_type,
                        name: export_obj.name.clone(),
                        properties: export_obj.properties.clone(),
                        confidence: export_obj.confidence,
                        created_at: now_ts(),
                    })
                    .await?;
                stats.objects_created += 1;
                id
            }
        };
        obj_id_by_key.insert(key, obj_id);
    }

    // Evidence: reuse by (doc_title, start, end, content) via the doc's first
    // chapter. Preload every involved doc's existing evidence ONCE instead of
    // calling `list_evidence_by_document` per bundle row (O(n²) → O(n)); the
    // cache is updated as rows are created so duplicate rows within one
    // bundle also dedupe. The key includes the SPAN: the same sentence text
    // at two offsets (repeated paragraph) is two distinct rows — content-only
    // dedup would silently drop the second occurrence and strand any profile
    // anchored to it (span-aware cache key, same rule as evidence_id).
    let mut evidence_cache: EvidenceCache = std::collections::HashMap::new();
    for &doc_id in docs_by_title.values() {
        let rows = store
            .list_evidence_by_document(doc_id)
            .await?
            .into_iter()
            .map(|e| (e.start_offset, e.end_offset, e.content))
            .collect();
        evidence_cache.insert(doc_id, rows);
    }
    for export_ev in &bundle.evidence {
        let Some(&doc_id) = docs_by_title.get(&export_ev.doc_title) else {
            continue;
        };
        let chapter_id = ensure_chapter(store, doc_id).await?;
        let existing = evidence_cache
            .get_mut(&doc_id)
            .expect("every bundle doc is preloaded above");
        let key = (
            export_ev.start_offset,
            export_ev.end_offset,
            export_ev.content.clone(),
        );
        if !existing.contains(&key) {
            store
                .create_evidence(&Evidence {
                    id: 0,
                    doc_id,
                    chapter_id,
                    start_offset: export_ev.start_offset,
                    end_offset: export_ev.end_offset,
                    content: export_ev.content.clone(),
                    created_at: now_ts(),
                })
                .await?;
            existing.insert(key);
            stats.evidence_created += 1;
        }
    }

    // Edges: resolve endpoints by (doc_title, name), skipping duplicates.
    let mut seen_edges: std::collections::HashSet<(String, String, String, String)> =
        std::collections::HashSet::new();
    for export_edge in &bundle.edges {
        let Some(&source_id) = obj_id_by_key.get(&(
            export_edge.doc_title.clone(),
            export_edge.source_name.clone(),
        )) else {
            continue;
        };
        let Some(&target_id) = obj_id_by_key.get(&(
            export_edge.doc_title.clone(),
            export_edge.target_name.clone(),
        )) else {
            continue;
        };
        let dedup_key = (
            export_edge.doc_title.clone(),
            export_edge.source_name.clone(),
            export_edge.predicate.clone(),
            export_edge.target_name.clone(),
        );
        if !seen_edges.insert(dedup_key) {
            continue;
        }
        // Idempotent re-import: skip when the store already has this
        // (source, target, predicate) edge — `create_edge` is a bare INSERT
        // with no UNIQUE, so a second import of the same bundle used to
        // double every relation.
        let already_exists = store
            .get_edges_touching(source_id)
            .await?
            .iter()
            .any(|e| e.target_id == target_id && e.predicate == export_edge.predicate);
        if already_exists {
            continue;
        }
        let origin = Origin::from_str(&export_edge.origin).unwrap_or(Origin::Observed);
        store
            .create_edge(&KnowledgeEdge {
                id: 0,
                source_id,
                target_id,
                predicate: export_edge.predicate.clone(),
                properties: export_edge.properties.clone(),
                origin,
                confidence: export_edge.confidence,
                valid_from: export_edge.valid_from,
                valid_to: export_edge.valid_to,
                created_at: now_ts(),
            })
            .await?;
        stats.edges_created += 1;
    }

    // Evidence links: resolve the object by name and the evidence by content
    // (both globally), then link idempotently via `link_evidence`.
    for link in &bundle.evidence_links {
        let Some(obj_id) = global_object_by_name(store, &link.source_name).await? else {
            continue;
        };
        let Some(ev_id) = global_evidence_by_content(store, &link.evidence_content).await? else {
            continue;
        };
        store
            .link_evidence(EvidenceSourceType::Object, obj_id, ev_id)
            .await?;
        stats.links_created += 1;
    }

    // V7 world model: upsert entities/profiles/relations by identity so a
    // re-import is a no-op (same-named entity and same (entity,key) profile
    // are reused rather than duplicated).
    let mut world_id_by_name: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    for w in &bundle.world_entities {
        let id = store
            .upsert_world_entity(&w.name, &w.entity_type, w.importance)
            .await?;
        world_id_by_name.insert(w.name.clone(), id);
    }
    for p in &bundle.world_profiles {
        let Some(&entity_id) = world_id_by_name.get(&p.entity_name) else {
            continue;
        };
        // Re-anchor the claim by content+span: evidence rows were already
        // imported (or exist in the store) with valid doc/chapter FKs — never
        // create a doc_id:0 row here (that violates the documents FK). When
        // the bundle carries a span, prefer the row at the SAME offset so a
        // repeated sentence re-attaches to its own occurrence; old bundles
        // (span = None) fall back to content-only matching.
        let evidence_id = match p.evidence_content.as_deref() {
            Some(content) => {
                global_evidence_by_anchor(store, content, p.evidence_start, p.evidence_end).await?
            }
            None => None,
        };
        store
            .upsert_world_profile(entity_id, &p.key, &p.value, p.confidence, evidence_id)
            .await?;
    }
    for r in &bundle.world_relations {
        let (Some(&source_id), Some(&target_id)) = (
            world_id_by_name.get(&r.source_name),
            world_id_by_name.get(&r.target_name),
        ) else {
            continue;
        };
        store
            .upsert_world_relation(source_id, target_id, &r.relation_type, r.confidence)
            .await?;
    }

    // T9: narrative events + states run after entities/profiles so anchors
    // resolve against rows this import already materialized.
    crate::knowledge::memory_export_world::import_world_sections(
        store,
        &bundle.world_events,
        &bundle.world_states,
    )
    .await?;

    Ok(stats)
}

/// Ensure a document has at least one chapter (so evidence FK constraints
/// hold); returns that chapter's id.
async fn ensure_chapter(store: &dyn KnowledgeStore, doc_id: i64) -> Result<i64> {
    if let Some(ch) = store.get_chapter_by_no(doc_id, 0).await? {
        return Ok(ch.id);
    }
    let id = store
        .create_chapter(&crate::knowledge::Chapter {
            id: 0,
            doc_id,
            chapter_no: 0,
            title: None,
            content: String::new(),
            start_offset: None,
            end_offset: None,
        })
        .await?;
    Ok(id)
}

/// Find an object by name across all documents (used when a link references
/// an object whose document isn't named explicitly in the bundle).
async fn global_object_by_name(store: &dyn KnowledgeStore, name: &str) -> Result<Option<i64>> {
    for doc in store.list_documents().await? {
        if let Some(obj) = store.find_object_by_name(name, Some(doc.id)).await? {
            return Ok(Some(obj.id));
        }
    }
    Ok(None)
}

/// Find an evidence row id by its content across all documents.
async fn global_evidence_by_content(
    store: &dyn KnowledgeStore,
    content: &str,
) -> Result<Option<i64>> {
    for doc in store.list_documents().await? {
        for ev in store.list_evidence_by_document(doc.id).await? {
            if ev.content == content {
                return Ok(Some(ev.id));
            }
        }
    }
    Ok(None)
}

/// Find an evidence row for a profile claim, preferring the row at the
/// exported `(start, end)` span, then falling back to content-only matching.
///
/// The span preference matters when the same sentence text appears at two
/// offsets (repeated paragraph): content alone would re-attach the claim to
/// the wrong occurrence after a restore.
async fn global_evidence_by_anchor(
    store: &dyn KnowledgeStore,
    content: &str,
    start: Option<i64>,
    end: Option<i64>,
) -> Result<Option<i64>> {
    let mut content_fallback = None;
    for doc in store.list_documents().await? {
        for ev in store.list_evidence_by_document(doc.id).await? {
            if ev.content != content {
                continue;
            }
            if start.is_some() && ev.start_offset == start && ev.end_offset == end {
                return Ok(Some(ev.id));
            }
            if content_fallback.is_none() {
                content_fallback = Some(ev.id);
            }
        }
    }
    Ok(content_fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::SQLiteKnowledgeStore;

    fn sample_bundle() -> ExportBundle {
        ExportBundle {
            format: EXPORT_FORMAT.to_string(),
            version: EXPORT_VERSION,
            exported_at: now_ts(),
            documents: vec![ExportDocument {
                title: "会话-导出".into(),
                author: None,
                doc_type: Some("dialog".into()),
                source: String::new(),
            }],
            objects: vec![ExportObject {
                doc_title: "会话-导出".into(),
                object_type: "person".into(),
                name: "用户".into(),
                properties: serde_json::json!({"偏好": "简洁"}),
                confidence: 0.8,
            }],
            edges: vec![ExportEdge {
                doc_title: "会话-导出".into(),
                source_name: "用户".into(),
                predicate: "偏好".into(),
                target_name: "简洁".into(),
                properties: serde_json::json!({}),
                origin: "observed".into(),
                confidence: 0.7,
                valid_from: None,
                valid_to: None,
            }],
            evidence: vec![ExportEvidence {
                doc_title: "会话-导出".into(),
                content: "我偏好简洁的架构。".into(),
                start_offset: Some(0),
                end_offset: Some(10),
            }],
            evidence_links: vec![ExportEvidenceLink {
                source_name: "用户".into(),
                evidence_content: "我偏好简洁的架构。".into(),
            }],
            world_entities: vec![ExportWorldEntity {
                name: "用户".into(),
                entity_type: "person".into(),
                importance: 0.5,
            }],
            world_profiles: vec![ExportWorldProfile {
                entity_name: "用户".into(),
                key: "偏好".into(),
                value: "简洁".into(),
                confidence: 0.8,
                evidence_content: None,
                evidence_start: None,
                evidence_end: None,
            }],
            world_relations: vec![],
            world_events: vec![],
            world_states: vec![],
        }
    }

    /// Objective: Verify `export_store` round-trips — importing an exported
    /// bundle into a fresh store reproduces the same graph.
    /// Invariants: import returns created counts; the entity is queryable by
    /// name; re-import is a no-op (objects_created == 0).
    #[tokio::test]
    async fn export_import_round_trip() {
        let src = SQLiteKnowledgeStore::open_in_memory()
            .await
            .expect("src store");
        let bundle = import_bundle(&src, &sample_bundle()).await.expect("seed");
        assert_eq!(bundle.documents_created, 1, "document created");
        assert_eq!(bundle.objects_created, 1, "object created");

        // Now export what we just imported.
        let exported = export_store(&src).await.expect("export");
        assert!(!exported.documents.is_empty(), "documents exported");
        assert!(!exported.objects.is_empty(), "objects exported");

        // Import into a fresh store and verify content is reproducible.
        let dst = SQLiteKnowledgeStore::open_in_memory()
            .await
            .expect("dst store");
        let stats = import_bundle(&dst, &exported).await.expect("re-import");
        assert_eq!(stats.objects_created, 1, "object recreated in fresh store");

        let obj = dst
            .find_object_by_name("用户", None)
            .await
            .expect("query")
            .expect("user entity present");
        assert_eq!(obj.properties["偏好"], "简洁", "properties preserved");

        // Re-import into the same store must be a no-op for objects.
        let again = import_bundle(&dst, &exported).await.expect("import again");
        assert_eq!(again.objects_created, 0, "idempotent: no new objects");
        assert_eq!(again.objects_merged, 1, "existing object merged instead");
    }

    /// Objective: Verify an unsupported format is rejected on import.
    /// Invariants: wrong format → error mentioning the format.
    #[tokio::test]
    async fn import_rejects_unknown_format() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let mut bundle = sample_bundle();
        bundle.format = "other-tool".into();
        let err = import_bundle(&store, &bundle)
            .await
            .expect_err("must reject");
        assert!(
            err.to_string().contains("unsupported export format"),
            "clear error, got: {err}"
        );
    }

    /// Objective: Verify a bundle from a FUTURE format version is rejected —
    /// silently importing v2-as-v1 would drop fields this build cannot read.
    /// Invariants: version > EXPORT_VERSION → error naming the version;
    /// version == EXPORT_VERSION still imports (covered by round-trip test).
    #[tokio::test]
    async fn import_rejects_newer_bundle_version() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let mut bundle = sample_bundle();
        bundle.version = EXPORT_VERSION + 1;
        let err = import_bundle(&store, &bundle)
            .await
            .expect_err("must reject");
        assert!(
            err.to_string().contains("version"),
            "error must name the version mismatch, got: {err}"
        );
    }

    /// Objective: Verify exporting an empty store yields an empty bundle, not
    /// an error.
    /// Invariants: empty store → all sections empty.
    #[tokio::test]
    async fn export_empty_store_is_empty() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let bundle = export_store(&store).await.expect("export");
        assert!(bundle.documents.is_empty());
        assert!(bundle.objects.is_empty());
        assert!(bundle.edges.is_empty());
        assert!(bundle.evidence.is_empty());
        assert!(bundle.evidence_links.is_empty());
    }

    /// Objective: Verify a world profile's evidence anchor exports its byte
    /// SPAN (not just content), so a restore can re-attach the claim to the
    /// right occurrence when the same sentence text appears at two offsets.
    /// Invariants: evidence_start/end equal the source row's span; a profile
    /// with no evidence keeps all three fields None.
    #[tokio::test]
    async fn profile_export_carries_evidence_span() {
        let store = SQLiteKnowledgeStore::open_in_memory().await.expect("store");
        let did = store
            .create_document(&crate::knowledge::Document {
                id: 0,
                title: "span-doc".into(),
                author: None,
                doc_type: Some("text".into()),
                source: String::new(),
                created_at: now_ts(),
            })
            .await
            .expect("doc");
        let cid = store
            .create_chapter(&crate::knowledge::Chapter {
                id: 0,
                doc_id: did,
                chapter_no: 1,
                title: None,
                content: String::new(),
                start_offset: None,
                end_offset: None,
            })
            .await
            .expect("chapter");
        // Same sentence text at two different offsets (repeated paragraph).
        store
            .create_evidence(&crate::knowledge::Evidence {
                id: 0,
                doc_id: did,
                chapter_id: cid,
                start_offset: Some(10),
                end_offset: Some(20),
                content: "重复的句子。".into(),
                created_at: now_ts(),
            })
            .await
            .expect("ev a");
        let ev_b = store
            .create_evidence(&crate::knowledge::Evidence {
                id: 0,
                doc_id: did,
                chapter_id: cid,
                start_offset: Some(300),
                end_offset: Some(310),
                content: "重复的句子。".into(),
                created_at: now_ts(),
            })
            .await
            .expect("ev b");
        let eid = store
            .upsert_world_entity("孔明", "person", 0.9)
            .await
            .expect("entity");
        // Anchor to the SECOND occurrence — content-only matching would pick
        // ev_a (first scan hit) and silently move the claim.
        store
            .upsert_world_profile(eid, "status", "出师", 0.8, Some(ev_b))
            .await
            .expect("profile");

        let bundle = export_store(&store).await.expect("export");
        let p = bundle
            .world_profiles
            .iter()
            .find(|p| p.key == "status")
            .expect("profile exported");
        assert_eq!(
            p.evidence_start,
            Some(300),
            "export must carry the anchored span, got {:?}",
            p.evidence_start
        );
        assert_eq!(p.evidence_end, Some(310));
        assert_eq!(
            p.evidence_content.as_deref(),
            Some("重复的句子。"),
            "content still exported"
        );

        // Import into a fresh store: the claim must re-attach to the row at
        // the SAME span (300..310), not to the first occurrence (10..20).
        let dst = SQLiteKnowledgeStore::open_in_memory().await.expect("dst");
        import_bundle(&dst, &bundle).await.expect("import");
        let profiles = dst.list_world_profiles().await.expect("list");
        let restored = profiles
            .iter()
            .find(|p| p.key == "status")
            .expect("restored profile");
        let evidence_id = restored
            .evidence_id
            .expect("restored profile keeps its anchor");
        // Locate the restored evidence row and check its span.
        let mut found_span = None;
        for doc in dst.list_documents().await.expect("docs") {
            for ev in dst.list_evidence_by_document(doc.id).await.expect("ev") {
                if ev.id == evidence_id {
                    found_span = Some((ev.start_offset, ev.end_offset));
                }
            }
        }
        assert_eq!(
            found_span,
            Some((Some(300), Some(310))),
            "restore must re-anchor to the span-matching row"
        );
    }
}
