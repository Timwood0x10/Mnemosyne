//! Writer — Phase 8.
//!
//! Converts a [`CompileResult`] into storage model types and writes them via
//! a [`KnowledgeStore`]. This is the only component that couples the compiler
//! pipeline to a specific storage backend.
//!
//! ## Responsibilities
//!
//! 1. **Document management** — create or find the document by title.
//! 2. **Object UPSERT** — for each [`CompiledObject`], find or create a
//!    [`KnowledgeObject`] by name + doc_id.
//! 3. **Edge insert** — for each [`CompiledEdge`], insert a [`KnowledgeEdge`]
//!    using the resolved object IDs from step 2.
//! 4. **Evidence insert** — create [`Evidence`] rows and link them to
//!    objects/edges via [`knowledge_evidence`].

use std::collections::HashMap;

use serde_json::json;

use crate::compiler::{CompileResult, CompiledObject};
use crate::error::Result;
use crate::knowledge::store::KnowledgeStore;
use crate::knowledge::{
    CompilerRun, Evidence, EvidenceSourceType, KnowledgeEdge, KnowledgeObject, ObjectType,
};

/// Write a [`CompileResult`] into the provided [`KnowledgeStore`].
///
/// Returns the [`CompilerRun`] id on success.
pub async fn write(
    result: &CompileResult,
    doc_title: &str,
    store: &dyn KnowledgeStore,
) -> Result<i64> {
    // 1. Document
    let doc_id = get_or_create_doc(store, doc_title).await?;

    // 2. Start compiler run
    let run_id = store
        .create_run(&CompilerRun {
            id: 0,
            doc_id,
            version: env!("CARGO_PKG_VERSION").to_string(),
            started_at: Some(now_ts()),
            finished_at: None,
            status: Some("running".into()),
            statistics: None,
        })
        .await?;

    // 3. Object UPSERT — build name → id map
    let mut object_ids: HashMap<String, i64> = HashMap::new();
    for obj in &result.objects {
        let id = upsert_object(store, obj, doc_id).await?;
        object_ids.insert(obj.name.clone(), id);
        object_ids.insert(obj.name.clone(), id);
    }

    // 4. Edge insert
    for edge in &result.edges {
        let Some(&source_id) = object_ids.get(&edge.source) else {
            continue;
        };
        let Some(&target_id) = object_ids.get(&edge.target) else {
            continue;
        };

        let ke = KnowledgeEdge {
            id: 0,
            source_id,
            target_id,
            predicate: edge.predicate.clone(),
            properties: json!({}),
            origin: edge.origin,
            confidence: edge.confidence,
            valid_from: None,
            valid_to: None,
            created_at: now_ts(),
        };
        let edge_id = store.create_edge(&ke).await?;

        // Evidence for the edge
        if !edge.evidence.text.is_empty() {
            let ev_id = store
                .create_evidence(&Evidence {
                    id: 0,
                    doc_id,
                    chapter_id: edge.evidence.segment_num as i64,
                    start_offset: Some(edge.evidence.offset_start as i64),
                    end_offset: Some(edge.evidence.offset_end as i64),
                    content: edge.evidence.text.clone(),
                    created_at: now_ts(),
                })
                .await?;
            store
                .link_evidence(EvidenceSourceType::Edge, edge_id, ev_id)
                .await?;
        }
    }

    // 5. Finish compiler run
    let stats = json!({
        "objects": result.stats.objects,
        "edges": result.stats.edges,
        "observations": result.stats.observations,
        "derived_edges": result.stats.derived_edges,
    });
    store.finish_run(run_id, "completed", &stats).await?;

    Ok(run_id)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Find an existing document by title, or create a new one.
async fn get_or_create_doc(store: &dyn KnowledgeStore, title: &str) -> Result<i64> {
    match store.find_document_by_title(title).await? {
        Some(doc) => Ok(doc.id),
        None => {
            let id = store
                .create_document(&crate::knowledge::Document {
                    id: 0,
                    title: title.to_owned(),
                    author: None,
                    doc_type: Some("compiled".into()),
                    created_at: now_ts(),
                })
                .await?;
            Ok(id)
        }
    }
}

/// Find an existing object by name + doc_id, or create one.
async fn upsert_object(
    store: &dyn KnowledgeStore,
    obj: &CompiledObject,
    doc_id: i64,
) -> Result<i64> {
    // Try to find existing object with same name in this document
    if let Some(existing) = store.find_object_by_name(&obj.name, Some(doc_id)).await? {
        return Ok(existing.id);
    }

    // Create new object
    let ko = KnowledgeObject {
        id: 0,
        doc_id,
        object_type: parse_object_type(&obj.object_type),
        name: obj.name.clone(),
        properties: json!(obj.properties),
        confidence: 1.0,
        created_at: now_ts(),
    };

    let id = store.create_object(&ko).await?;

    // Evidence for the object
    if !obj.evidence.text.is_empty() {
        let ev_id = store
            .create_evidence(&Evidence {
                id: 0,
                doc_id,
                chapter_id: obj.evidence.segment_num as i64,
                start_offset: Some(obj.evidence.offset_start as i64),
                end_offset: Some(obj.evidence.offset_end as i64),
                content: obj.evidence.text.clone(),
                created_at: now_ts(),
            })
            .await?;
        store
            .link_evidence(EvidenceSourceType::Object, id, ev_id)
            .await?;
    }

    Ok(id)
}

/// Parse an object_type string into the typed enum.
fn parse_object_type(s: &str) -> ObjectType {
    match s {
        "person" => ObjectType::Person,
        "event" => ObjectType::Event,
        "place" | "location" => ObjectType::Place,
        "organization" => ObjectType::Organization,
        "artifact" => ObjectType::Artifact,
        "concept" => ObjectType::Concept,
        "role" => ObjectType::Role,
        _ => ObjectType::Concept,
    }
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
