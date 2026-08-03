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

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::knowledge::store::KnowledgeStore;
use crate::knowledge::{
    Document, Evidence, EvidenceSourceType, KnowledgeEdge, KnowledgeObject, ObjectType, Origin,
};

/// Format tag embedded in every bundle, for validation on import.
pub const EXPORT_FORMAT: &str = "lorescope-memory";
/// Current snapshot format version (bump on any schema-affecting change).
pub const EXPORT_VERSION: u32 = 1;

/// A document row, keyed for dedup by `title`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportDocument {
    pub title: String,
    pub author: Option<String>,
    pub doc_type: Option<String>,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportEvidence {
    pub doc_title: String,
    pub content: String,
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

    for doc in store.list_documents().await? {
        documents.push(ExportDocument {
            title: doc.title.clone(),
            author: doc.author.clone(),
            doc_type: doc.doc_type.clone(),
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
            evidence.push(ExportEvidence {
                doc_title: doc.title.clone(),
                content: ev.content.clone(),
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
            name_by_id.get(&p.entity_id).map(|name| ExportWorldProfile {
                entity_name: name.clone(),
                key: p.key,
                value: p.value,
                confidence: p.confidence,
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
    })
}

/// Resolve the object names at both ends of an edge, by querying the store.
async fn resolve_edge_endpoint_names(
    store: &dyn KnowledgeStore,
    edge: &KnowledgeEdge,
) -> Result<Option<(String, String)>> {
    let src = store.get_object(edge.source_id).await?;
    let tgt = store.get_object(edge.target_id).await?;
    match (src, tgt) {
        (Some(s), Some(t)) => Ok(Some((s.name, t.name))),
        _ => Ok(None),
    }
}

/// Replay a [`ExportBundle`] into `store`, deduplicating by identity.
///
/// Objects with an existing `(doc_title, name)` get their properties merged;
/// documents/evidence/edges that already exist are reused (a re-import is a
/// no-op for them). Links are idempotent.
///
/// # Errors
///
/// Delegates to the store's write errors.
pub async fn import_bundle(
    store: &dyn KnowledgeStore,
    bundle: &ExportBundle,
) -> Result<ImportStats> {
    if bundle.format != EXPORT_FORMAT {
        return Err(crate::error::Error::InvalidInput(format!(
            "unsupported export format `{}` (expected `{EXPORT_FORMAT}`)",
            bundle.format
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

    // Group records by document title so doc-scoped lookups stay local.
    let mut docs_by_title: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    for export_doc in &bundle.documents {
        let doc_id = match store.find_document_by_title(&export_doc.title).await? {
            Some(existing) => existing.id,
            None => {
                let id = store
                    .create_document(&Document {
                        id: 0,
                        title: export_doc.title.clone(),
                        author: export_doc.author.clone(),
                        doc_type: export_doc.doc_type.clone(),
                        created_at: now_ts(),
                    })
                    .await?;
                stats.documents_created += 1;
                // Guarantee a chapter so evidence FK constraints hold.
                ensure_chapter(store, id).await?;
                id
            }
        };
        docs_by_title.insert(export_doc.title.clone(), doc_id);
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

    // Evidence: reuse by (doc_title, content) via the doc's first chapter.
    for export_ev in &bundle.evidence {
        let Some(&doc_id) = docs_by_title.get(&export_ev.doc_title) else {
            continue;
        };
        let chapter_id = ensure_chapter(store, doc_id).await?;
        let exists = store
            .list_evidence_by_document(doc_id)
            .await?
            .iter()
            .any(|e| e.content == export_ev.content);
        if !exists {
            store
                .create_evidence(&Evidence {
                    id: 0,
                    doc_id,
                    chapter_id,
                    start_offset: None,
                    end_offset: None,
                    content: export_ev.content.clone(),
                    created_at: now_ts(),
                })
                .await?;
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
        store
            .upsert_world_profile(entity_id, &p.key, &p.value, p.confidence)
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
            }],
            world_relations: vec![],
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
}
