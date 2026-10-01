//! Typed evidence links for the export bundle: object→evidence AND
//! edge→evidence.
//!
//! v1/v2 bundles carried only object links (and silently dropped every
//! `EvidenceSourceType::Edge` row — `Migrator::migrate` writes those, so a
//! backup/restore lost them: the "export lossiness" backlog item). v3 makes
//! the link kind explicit: `source_type` (defaulted so v1/v2 JSON still
//! deserializes) plus an [`ExportEdgeKey`] locating the edge portably by
//! `(doc_title, source, predicate, target)` — numeric ids never travel.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::knowledge::memory_export::{global_evidence_by_anchor, global_object_by_name};
use crate::knowledge::store::KnowledgeStore;
use crate::knowledge::{EvidenceSourceType, KnowledgeEdge};

/// Portable identity of an edge: the same key the edge import dedups on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ExportEdgeKey {
    /// Title of the document the edge belongs to.
    pub doc_title: String,
    /// Source object name.
    pub source_name: String,
    /// Relation predicate.
    pub predicate: String,
    /// Target object name.
    pub target_name: String,
}

/// One evidence link in a bundle, typed by `source_type`.
///
/// `"object"` (the default, so v1/v2 bundles without the field keep their
/// behaviour) resolves through `source_name`; `"edge"` resolves through the
/// `edge` key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportEvidenceLink {
    /// Link kind: `"object"` or `"edge"`. Defaulted for old bundles.
    #[serde(default = "default_source_type")]
    pub source_type: String,
    /// Object name for object links (edge links reuse it for the edge's
    /// source name — informational only; resolution uses `edge`).
    pub source_name: String,
    /// Edge identity for edge links; `None` for object links.
    #[serde(default)]
    pub edge: Option<ExportEdgeKey>,
    /// Content of the evidence row this link points at.
    pub evidence_content: String,
    /// Source byte span of the evidence row, so import can re-attach to the
    /// SAME occurrence of a repeated sentence.
    ///
    /// Optional because bundles written before this field exist: a link without
    /// a span falls back to content-only resolution, which is what every bundle
    /// used to do.
    #[serde(default)]
    pub evidence_start: Option<i64>,
    /// Source byte span end (see [`Self::evidence_start`]).
    #[serde(default)]
    pub evidence_end: Option<i64>,
}

fn default_source_type() -> String {
    "object".to_string()
}

/// Resolve the object names at both ends of an edge, querying the store.
///
/// Returns `None` when either endpoint object no longer exists (dangling
/// edge) so the export can skip it instead of writing a broken edge.
pub(crate) async fn resolve_edge_endpoint_names(
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

/// Export every evidence link with its type: object links carry the object
/// name, edge links carry the portable edge key.
///
/// Edge context is gathered by walking every document's edges once (the
/// store has no get-by-id edge API); dangling edges — an endpoint object
/// that no longer exists — are skipped, mirroring the edge export itself.
pub(crate) async fn export_evidence_links(
    store: &dyn KnowledgeStore,
) -> Result<Vec<ExportEvidenceLink>> {
    // edge id → (doc_title, source, predicate, target)
    let mut edge_info: HashMap<i64, ExportEdgeKey> = HashMap::new();
    for doc in store.list_documents().await? {
        for edge in store.list_edges_by_document(doc.id).await? {
            if let Some((source_name, target_name)) =
                resolve_edge_endpoint_names(store, &edge).await?
            {
                edge_info.insert(
                    edge.id,
                    ExportEdgeKey {
                        doc_title: doc.title.clone(),
                        source_name,
                        predicate: edge.predicate.clone(),
                        target_name,
                    },
                );
            }
        }
    }

    let mut out = Vec::new();
    for link in store.list_evidence_links().await? {
        match link.source_type {
            EvidenceSourceType::Object => {
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
                out.push(ExportEvidenceLink {
                    source_type: "object".to_string(),
                    source_name: obj.name.clone(),
                    edge: None,
                    evidence_content: ev.content.clone(),
                    evidence_start: ev.start_offset,
                    evidence_end: ev.end_offset,
                });
            }
            EvidenceSourceType::Edge => {
                let Some(key) = edge_info.get(&link.source_id) else {
                    continue;
                };
                let Some(ev) = store
                    .get_evidence_for(EvidenceSourceType::Edge, link.source_id)
                    .await?
                    .into_iter()
                    .find(|e| e.id == link.evidence_id)
                else {
                    continue;
                };
                out.push(ExportEvidenceLink {
                    source_type: "edge".to_string(),
                    source_name: key.source_name.clone(),
                    edge: Some(key.clone()),
                    evidence_content: ev.content.clone(),
                    evidence_start: ev.start_offset,
                    evidence_end: ev.end_offset,
                });
            }
        }
    }
    Ok(out)
}

/// Import evidence links, resolving each target id from the given maps.
///
/// `edge_id_by_key` is built by the caller's edge import pass (which both
/// creates and reuses edges); a link whose target cannot be resolved is
/// skipped rather than mis-typed — a v1/v2 bundle (`source_type` defaulted
/// to `"object"`) takes exactly the old object path.
///
/// Returns `(created, skipped)`: idempotent re-imports resolve to existing rows
/// and count again (matching the previous `links_created` semantics), while a
/// link whose evidence or endpoint cannot be resolved is SKIPPED — and counted,
/// so a lossy restore is visible to the caller instead of only in a log.
pub(crate) async fn import_evidence_links(
    store: &dyn KnowledgeStore,
    links: &[ExportEvidenceLink],
    edge_id_by_key: &HashMap<ExportEdgeKey, i64>,
) -> Result<(usize, usize)> {
    let mut created = 0;
    let mut skipped = 0;
    for link in links {
        // Resolve by SPAN first: when the same sentence occurs twice, content
        // alone re-attaches the link to the wrong occurrence (the object and
        // edge endpoints are name-keyed, so the mistake is invisible). A bundle
        // written before the span fields existed falls back to content.
        let Some(ev_id) = global_evidence_by_anchor(
            store,
            &link.evidence_content,
            link.evidence_start,
            link.evidence_end,
        )
        .await?
        else {
            skipped += 1;
            continue;
        };
        match link.source_type.as_str() {
            "edge" => {
                let Some(key) = &link.edge else {
                    skipped += 1;
                    continue;
                };
                let Some(&edge_id) = edge_id_by_key.get(key) else {
                    skipped += 1;
                    continue;
                };
                store
                    .link_evidence(EvidenceSourceType::Edge, edge_id, ev_id)
                    .await?;
                created += 1;
            }
            // "object" and the v1/v2 default both resolve through source_name.
            _ => {
                let Some(obj_id) = global_object_by_name(store, &link.source_name).await? else {
                    skipped += 1;
                    continue;
                };
                store
                    .link_evidence(EvidenceSourceType::Object, obj_id, ev_id)
                    .await?;
                created += 1;
            }
        }
    }
    Ok((created, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::SQLiteKnowledgeStore;
    use crate::knowledge::memory_export::{EXPORT_VERSION, export_store, import_bundle};
    use crate::knowledge::{Chapter, Document, Evidence, KnowledgeObject, ObjectType, Origin};

    fn now_ts() -> i64 {
        chrono::Utc::now().timestamp()
    }

    /// Seed one document with two objects, an edge between them, and one
    /// evidence row linked to BOTH the edge and one object.
    async fn seed(store: &SQLiteKnowledgeStore) {
        let doc_id = store
            .create_document(&Document {
                id: 0,
                title: "关系图".into(),
                author: None,
                doc_type: Some("text".into()),
                source: "seed".into(),
                created_at: now_ts(),
            })
            .await
            .expect("doc");
        let chapter_id = store
            .create_chapter(&Chapter {
                id: 0,
                doc_id,
                chapter_no: 1,
                title: None,
                content: String::new(),
                start_offset: None,
                end_offset: None,
            })
            .await
            .expect("chapter");
        let mk = |name: &str| KnowledgeObject {
            id: 0,
            doc_id,
            object_type: ObjectType::Person,
            name: name.into(),
            properties: serde_json::json!({}),
            confidence: 0.9,
            created_at: now_ts(),
        };
        let a = store.create_object(&mk("甲")).await.expect("object a");
        let b = store.create_object(&mk("乙")).await.expect("object b");
        let edge_id = store
            .create_edge(&KnowledgeEdge {
                id: 0,
                source_id: a,
                target_id: b,
                predicate: "结义".into(),
                properties: serde_json::json!({}),
                origin: Origin::Observed,
                confidence: 0.8,
                valid_from: Some(1),
                valid_to: None,
                created_at: now_ts(),
            })
            .await
            .expect("edge");
        let ev_id = store
            .create_evidence(&Evidence {
                id: 0,
                doc_id,
                chapter_id,
                start_offset: Some(10),
                end_offset: Some(20),
                content: "二人结义，誓同生死。".into(),
                created_at: now_ts(),
            })
            .await
            .expect("evidence");
        store
            .link_evidence(EvidenceSourceType::Edge, edge_id, ev_id)
            .await
            .expect("edge link");
        store
            .link_evidence(EvidenceSourceType::Object, a, ev_id)
            .await
            .expect("object link");
    }

    /// Objective: Verify BOTH link types survive export → import — edge
    /// links were silently dropped before v3 (the export-lossiness gap:
    /// migrate() writes them, backup lost them).
    /// Invariants: bundle carries source_type "object" AND "edge" with a
    /// resolvable edge key; a fresh store ends up with both link rows;
    /// bundle version == EXPORT_VERSION (v3).
    #[tokio::test]
    async fn edge_and_object_links_round_trip() {
        let src = SQLiteKnowledgeStore::open_in_memory().await.expect("src");
        seed(&src).await;

        let bundle = export_store(&src).await.expect("export");
        assert_eq!(bundle.version, EXPORT_VERSION, "bundle is current version");
        assert_eq!(bundle.evidence_links.len(), 2, "both links exported");

        let edge_link = bundle
            .evidence_links
            .iter()
            .find(|l| l.source_type == "edge")
            .expect("edge link exported");
        let key = edge_link.edge.as_ref().expect("edge key present");
        assert_eq!(
            key.doc_title, "关系图",
            "the link key must carry its document title"
        );
        assert_eq!(
            key.source_name, "甲",
            "the link key must carry its source name"
        );
        assert_eq!(
            key.predicate, "结义",
            "the link key must carry its predicate"
        );
        assert_eq!(
            key.target_name, "乙",
            "the link key must carry its target name"
        );
        let obj_link = bundle
            .evidence_links
            .iter()
            .find(|l| l.source_type == "object")
            .expect("object link exported");
        assert_eq!(
            obj_link.source_name, "甲",
            "the exported link must keep its source name"
        );
        assert!(obj_link.edge.is_none(), "object link carries no edge key");

        let dst = SQLiteKnowledgeStore::open_in_memory().await.expect("dst");
        import_bundle(&dst, &bundle).await.expect("import");
        let docs = dst.list_documents().await.expect("docs");
        let doc_id = docs[0].id;
        let edges = dst.list_edges_by_document(doc_id).await.expect("edges");
        assert_eq!(edges.len(), 1, "edge imported");
        let objects = dst.list_objects_by_document(doc_id).await.expect("objects");
        let a = objects
            .iter()
            .find(|o| o.name == "甲")
            .expect("object 甲")
            .id;
        let from_edge = dst
            .get_evidence_for(EvidenceSourceType::Edge, edges[0].id)
            .await
            .expect("edge evidence");
        assert_eq!(from_edge.len(), 1, "edge link restored on the fresh store");
        let from_obj = dst
            .get_evidence_for(EvidenceSourceType::Object, a)
            .await
            .expect("object evidence");
        assert_eq!(from_obj.len(), 1, "object link restored too");
    }

    /// Objective: Verify v1/v2 JSON (no `source_type` / `edge` keys) still
    /// deserializes and takes the object path — the serde defaults keep old
    /// bundles importable after the v3 field additions.
    /// Invariants: default source_type == "object"; edge == None; the
    /// object link imports cleanly.
    #[tokio::test]
    async fn old_bundle_link_json_defaults_to_object() {
        let link: ExportEvidenceLink = serde_json::from_value(serde_json::json!({
            "source_name": "甲",
            "evidence_content": "二人结义，誓同生死。"
        }))
        .expect("v1/v2 link payload must deserialize");
        assert_eq!(
            link.source_type, "object",
            "missing kind defaults to object"
        );
        assert!(link.edge.is_none(), "missing edge key defaults to None");

        let src = SQLiteKnowledgeStore::open_in_memory().await.expect("src");
        seed(&src).await;
        let dst = SQLiteKnowledgeStore::open_in_memory().await.expect("dst");
        // Minimal v3-shaped bundle around the old link JSON: documents +
        // evidence rows first so the link can resolve.
        let full = export_store(&src).await.expect("export");
        let mut bundle = full;
        bundle.evidence_links = vec![link];
        import_bundle(&dst, &bundle).await.expect("import");
        let docs = dst.list_documents().await.expect("docs");
        let objects = dst
            .list_objects_by_document(docs[0].id)
            .await
            .expect("objects");
        let a = objects.iter().find(|o| o.name == "甲").expect("甲").id;
        let from_obj = dst
            .get_evidence_for(EvidenceSourceType::Object, a)
            .await
            .expect("object evidence");
        assert_eq!(from_obj.len(), 1, "old-shape object link imports");
    }
}
