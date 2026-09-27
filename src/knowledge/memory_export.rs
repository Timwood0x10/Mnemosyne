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
//! `objects`, `edges`, `evidence` with byte spans, typed evidence links
//! (object→evidence AND edge→evidence — v3; `Migrator::migrate` writes
//! edge links, so backups keep them), and the V7 world model —
//! `world_entities`, `world_profiles`, `world_relations`,
//! `world_events` (with participants), `world_states`.
//!
//! Not exported, by design:
//! - `chapters` bodies — the only bodies come from `Migrator::migrate`,
//!   which rebuilds them by re-reading the corpus files. `compile_source`
//!   never writes bodies at all: its chapter row is an empty shell kept
//!   solely for the evidence FK, so nothing is lost by omitting it.
//!
//! Known limitation: every sub-bundle row (objects, edges, evidence, links)
//! is keyed by `doc_title` alone, so a bundle holding same-titled documents
//! from different sources attaches those rows to the FIRST such document.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
pub use crate::knowledge::memory_export_mentions::ExportMention;
pub use crate::knowledge::memory_export_world::{
    ExportEventIdentity, ExportEventParticipant, ExportWorldEvent, ExportWorldState,
};
use crate::knowledge::store::KnowledgeStore;
use crate::knowledge::{Document, Evidence, KnowledgeEdge, KnowledgeObject, ObjectType, Origin};

/// Format tag embedded in every bundle, for validation on import.
pub const EXPORT_FORMAT: &str = "lorescope-memory";
/// Current snapshot format version (bump on any schema-affecting change).
///
/// v4 carries `mentions` (where each name occurs) — without it a restore
/// silently dropped the entity index; v3 typed the evidence links (object vs
/// edge, T19); v2 added the `world_events`/`world_states` sections (T9).
/// Older bundles still import (`#[serde(default)]` → empty sections, links
/// default to object), and the bump is deliberate: a v4 bundle holds data a v3
/// reader would drop without saying so, so it must refuse instead.
pub const EXPORT_VERSION: u32 = 4;

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

use crate::knowledge::memory_export_links::resolve_edge_endpoint_names;
pub use crate::knowledge::memory_export_links::{ExportEdgeKey, ExportEvidenceLink};

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
    /// Where each name occurs (T20): the entity index a restore used to drop.
    #[serde(default)]
    pub mentions: Vec<ExportMention>,
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
    pub mentions_created: usize,
    /// Rows the bundle REFERENCES but the store cannot resolve — a missing
    /// document, object, world entity or link endpoint. Counted so a caller can
    /// tell a clean restore from a lossy one instead of inferring it from a log.
    pub unresolved_references: usize,
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

    // Typed evidence links (object + edge) — see `memory_export_links`.
    let evidence_links =
        crate::knowledge::memory_export_links::export_evidence_links(store).await?;

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
    // Mentions travel last: they resolve their object by (doc_title, name).
    let mentions = crate::knowledge::memory_export_mentions::export_mentions(store).await?;

    Ok(ExportBundle {
        format: EXPORT_FORMAT.to_string(),
        version: EXPORT_VERSION,
        exported_at: now_ts(),
        documents,
        objects,
        edges,
        evidence,
        evidence_links,
        mentions,
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

/// Import a bundle into a store, deduplicating by identity.
///
/// Re-importing the same bundle converges: documents are matched by
/// `(title, source)`, objects by `(doc, name)`, evidence by
/// `(doc, span, content)`, edges by `(source, predicate, target)`, and every
/// world row by its identity index. Mentions are matched by
/// `(object, span, alias)`.
///
/// **Where an identity already exists, the bundle wins.** The non-identity
/// columns — object properties (merged key by key), confidence, importance,
/// descriptions, state values and spans — are refreshed from the bundle. This is
/// deliberate (a restore is supposed to reproduce the bundle it was handed, not
/// the store's drifted state), but it means importing an OLD bundle over a newer
/// store moves those values backwards instead of skipping the row. A deployment
/// that wants "never overwrite the store" must import into an empty database;
/// there is no per-field switch, by design, because a half-applied merge is
/// harder to reason about than either policy alone.
///
/// The whole import runs in one transaction (see below), so a failure leaves the
/// store exactly as it was.
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] for a bundle whose format tag is not ours or
/// whose version is newer than this build understands, and the first storage
/// error of the import (with the transaction rolled back).
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
    // A bundle is a snapshot, so the whole import runs in ONE transaction: a
    // failure halfway through (a constraint violation, a disk error) must not
    // leave the graph half-restored while the caller is told "import failed".
    //
    // The store's transaction API is not re-entrant (SQLite rejects a nested
    // `BEGIN`), so a caller that already owns a transaction keeps ownership of
    // it — including the commit.
    if store.in_transaction().await? {
        return import_bundle_body(store, bundle).await;
    }
    store.begin_transaction().await?;
    match import_bundle_body(store, bundle).await {
        Ok(stats) => {
            store.commit_transaction().await?;
            Ok(stats)
        }
        Err(error) => {
            if let Err(rollback) = store.rollback_transaction().await {
                // The caller needs the original failure; a failed rollback is
                // logged because it means the store may hold partial data.
                tracing::error!(
                    error = %rollback,
                    "rolling back a failed import failed; the store may hold part of the bundle"
                );
            }
            Err(error)
        }
    }
}

/// The import itself: validation and transaction handling live in
/// [`import_bundle`], so this only walks the bundle.
async fn import_bundle_body(
    store: &dyn KnowledgeStore,
    bundle: &ExportBundle,
) -> Result<ImportStats> {
    let mut stats = ImportStats {
        documents_created: 0,
        objects_created: 0,
        objects_merged: 0,
        evidence_created: 0,
        edges_created: 0,
        links_created: 0,
        mentions_created: 0,
        unresolved_references: 0,
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
            // Orphan object: the bundle names a document it does not carry.
            stats.unresolved_references += 1;
            continue;
        };
        let object_type = ObjectType::from_str(&export_obj.object_type).unwrap_or_else(|error| {
            // A bundle from a newer build can name a type this build does not
            // know. Coercion keeps the restore going, but silently rewriting the
            // value (the column even has a CHECK constraint) hid the mismatch.
            tracing::warn!(
                error = %error,
                value = %export_obj.object_type,
                "unknown object type in the bundle; storing it as `concept`"
            );
            ObjectType::Concept
        });
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

    // Mentions: the entity index (where each name occurs). Imported right after
    // the objects they point at, keyed by (doc_title, object_name) like every
    // other cross-database reference in a bundle.
    let (mentions_created, mention_skips) =
        crate::knowledge::memory_export_mentions::import_mentions(
            store,
            &bundle.mentions,
            &docs_by_title,
            &obj_id_by_key,
        )
        .await?;
    stats.mentions_created += mentions_created;
    stats.unresolved_references += mention_skips;

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
            stats.unresolved_references += 1;
            continue;
        };
        let chapter_id = ensure_chapter(store, doc_id).await?;
        let existing = evidence_cache.get_mut(&doc_id).ok_or_else(|| {
            // The cache is seeded from `docs_by_title.values()` above, so this
            // cannot happen today — but a panic inside a restore would abort the
            // caller for an invariant that a future edit can break.
            Error::Internal(format!("document {doc_id} missing from the evidence cache"))
        })?;
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
    // edge identity → local id, consumed by the evidence-link pass below.
    let mut edge_id_by_key: std::collections::HashMap<
        crate::knowledge::memory_export_links::ExportEdgeKey,
        i64,
    > = std::collections::HashMap::new();
    for export_edge in &bundle.edges {
        let Some(&source_id) = obj_id_by_key.get(&(
            export_edge.doc_title.clone(),
            export_edge.source_name.clone(),
        )) else {
            stats.unresolved_references += 1;
            continue;
        };
        let Some(&target_id) = obj_id_by_key.get(&(
            export_edge.doc_title.clone(),
            export_edge.target_name.clone(),
        )) else {
            stats.unresolved_references += 1;
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
        // double every relation. Keep its id: edge evidence links re-point
        // through this map.
        let already_exists = store
            .get_edges_touching(source_id)
            .await?
            .into_iter()
            .find(|e| e.target_id == target_id && e.predicate == export_edge.predicate);
        if let Some(existing) = already_exists {
            edge_id_by_key
                .entry(crate::knowledge::memory_export_links::ExportEdgeKey {
                    doc_title: export_edge.doc_title.clone(),
                    source_name: export_edge.source_name.clone(),
                    predicate: export_edge.predicate.clone(),
                    target_name: export_edge.target_name.clone(),
                })
                .or_insert(existing.id);
            continue;
        }
        let origin = Origin::from_str(&export_edge.origin).unwrap_or_else(|error| {
            tracing::warn!(
                error = %error,
                value = %export_edge.origin,
                "unknown edge origin in the bundle; storing it as `observed`"
            );
            Origin::Observed
        });
        let edge_id = store
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
        edge_id_by_key
            .entry(crate::knowledge::memory_export_links::ExportEdgeKey {
                doc_title: export_edge.doc_title.clone(),
                source_name: export_edge.source_name.clone(),
                predicate: export_edge.predicate.clone(),
                target_name: export_edge.target_name.clone(),
            })
            .or_insert(edge_id);
        stats.edges_created += 1;
    }

    // Typed evidence links: objects resolve by name, edges by the key the
    // edge pass above recorded (v1/v2 links default to the object path).
    let (links_created, link_skips) = crate::knowledge::memory_export_links::import_evidence_links(
        store,
        &bundle.evidence_links,
        &edge_id_by_key,
    )
    .await?;
    stats.links_created += links_created;
    stats.unresolved_references += link_skips;

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
            stats.unresolved_references += 1;
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
pub(crate) async fn ensure_chapter(store: &dyn KnowledgeStore, doc_id: i64) -> Result<i64> {
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
pub(crate) async fn global_object_by_name(
    store: &dyn KnowledgeStore,
    name: &str,
) -> Result<Option<i64>> {
    for doc in store.list_documents().await? {
        if let Some(obj) = store.find_object_by_name(name, Some(doc.id)).await? {
            return Ok(Some(obj.id));
        }
    }
    Ok(None)
}

/// Find an evidence row id by its content across all documents.
pub(crate) async fn global_evidence_by_content(
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

/// Find an evidence row for a claim, preferring the row at the exported
/// `(start, end)` span, then falling back to content-only matching.
///
/// The span preference matters when the same sentence text appears at two
/// offsets (repeated paragraph): content alone would re-attach the claim to the
/// wrong occurrence after a restore. Used by world profiles **and** by evidence
/// links (`memory_export_links`), so both paths re-anchor identically.
pub(crate) async fn global_evidence_by_anchor(
    store: &dyn KnowledgeStore,
    content: &str,
    start: Option<i64>,
    end: Option<i64>,
) -> Result<Option<i64>> {
    // Without a span there is nothing to prefer, so take the plain content
    // lookup (a bundle written before the span fields existed).
    if start.is_none() {
        return global_evidence_by_content(store, content).await;
    }
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
