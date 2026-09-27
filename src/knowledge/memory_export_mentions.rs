//! Mention rows in an export bundle: where each name occurs in the text.
//!
//! `mentions` is what makes an entity locatable — the object says *who*, the
//! mention says *where in the document*. It was left out of the bundle
//! entirely, so a backup/restore silently dropped the index while still
//! promising that "memory is never lost".
//!
//! Both directions are name-keyed: a bundle cannot carry local row ids, so a
//! mention travels as `(doc_title, object_name)` plus its span and alias.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::knowledge::Mention;
use crate::knowledge::memory_export::ensure_chapter;
use crate::knowledge::store::KnowledgeStore;

/// Export an object's mentions, keyed by document title.
pub(crate) async fn export_mentions(store: &dyn KnowledgeStore) -> Result<Vec<ExportMention>> {
    let mut out = Vec::new();
    for doc in store.list_documents().await? {
        for obj in store.list_objects_by_document(doc.id).await? {
            for mention in store.get_mentions_for_object(obj.id).await? {
                out.push(ExportMention {
                    doc_title: doc.title.clone(),
                    object_name: obj.name.clone(),
                    start_offset: mention.start_offset,
                    end_offset: mention.end_offset,
                    alias_used: mention.alias_used.clone(),
                    confidence: mention.confidence,
                });
            }
        }
    }
    Ok(out)
}

/// Import mentions, resolving their object and chapter locally.
///
/// Mentions are deduplicated against the rows the objects already carry:
/// `mentions` has no unique constraint and `create_mention` is a plain insert,
/// so without this a re-import would append a second copy of every mention
/// (the evidence pass solves the same problem with a span-keyed cache).
///
/// Returns `(created, skipped)`: a mention whose document or object the bundle
/// cannot resolve is skipped, and counted so the caller learns the restore was
/// lossy.
///
/// # Errors
///
/// Returns a storage error when a lookup, the chapter provisioning or the
/// insert fails; the caller runs inside a transaction, so a failure discards
/// the whole bundle.
pub(crate) async fn import_mentions(
    store: &dyn KnowledgeStore,
    mentions: &[ExportMention],
    docs_by_title: &HashMap<String, i64>,
    obj_id_by_key: &HashMap<(String, String), i64>,
) -> Result<(usize, usize)> {
    /// Identity of a mention inside its object: the span plus the alias used.
    type MentionKey = (Option<i64>, Option<i64>, Option<String>);

    let mut seen_by_object: HashMap<i64, HashSet<MentionKey>> = HashMap::new();
    let mut chapter_by_doc: HashMap<i64, i64> = HashMap::new();
    let mut created = 0usize;
    let mut skipped = 0usize;

    for mention in mentions {
        let Some(&doc_id) = docs_by_title.get(&mention.doc_title) else {
            // The bundle names a document it does not carry: skip rather than
            // attach the mention to some other document.
            skipped += 1;
            continue;
        };
        let key = (mention.doc_title.clone(), mention.object_name.clone());
        let Some(&object_id) = obj_id_by_key.get(&key) else {
            skipped += 1;
            continue;
        };
        let chapter_id = match chapter_by_doc.get(&doc_id) {
            Some(&id) => id,
            None => {
                let id = ensure_chapter(store, doc_id).await?;
                chapter_by_doc.insert(doc_id, id);
                id
            }
        };
        let seen = match seen_by_object.entry(object_id) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => entry.insert(
                store
                    .get_mentions_for_object(object_id)
                    .await?
                    .into_iter()
                    .map(|existing| {
                        (
                            existing.start_offset,
                            existing.end_offset,
                            existing.alias_used,
                        )
                    })
                    .collect(),
            ),
        };
        let identity = (
            mention.start_offset,
            mention.end_offset,
            mention.alias_used.clone(),
        );
        if !seen.insert(identity) {
            continue;
        }
        store
            .create_mention(&Mention {
                id: 0,
                object_id,
                chapter_id,
                start_offset: mention.start_offset,
                end_offset: mention.end_offset,
                alias_used: mention.alias_used.clone(),
                confidence: mention.confidence,
            })
            .await?;
        created += 1;
    }
    Ok((created, skipped))
}

/// One mention row, keyed by `(doc_title, object_name)` instead of local ids.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportMention {
    /// Document the mention occurs in (resolved by title on import).
    pub doc_title: String,
    /// Object the mention points at (resolved by name on import).
    pub object_name: String,
    /// Source byte span start of the mention.
    #[serde(default)]
    pub start_offset: Option<i64>,
    /// Source byte span end of the mention.
    #[serde(default)]
    pub end_offset: Option<i64>,
    /// The alias actually used at this occurrence, when it differs from the name.
    #[serde(default)]
    pub alias_used: Option<String>,
    /// Confidence recorded for this mention.
    #[serde(default = "default_confidence")]
    pub confidence: f64,
}

/// Confidence used for a mention in a bundle that omitted the field.
fn default_confidence() -> f64 {
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::{Document, KnowledgeObject, ObjectType};

    /// Objective: Verify a mention survives the round trip and that re-importing
    /// the same bundle converges. `mentions` was absent from the bundle
    /// entirely, and it has no unique constraint, so the naive fix (insert every
    /// row) would double the index on every restore.
    /// Invariants: one mention round-trips with its span and alias; a second
    /// import creates none; a bundle naming an unknown document/object is skipped.
    #[tokio::test]
    async fn mentions_round_trip_and_re_import_converges() {
        let store = crate::knowledge::SQLiteKnowledgeStore::open_in_memory()
            .await
            .expect("open store");
        let doc_id = store
            .create_document(&Document {
                id: 0,
                title: "三国".into(),
                author: None,
                doc_type: Some("text".into()),
                source: String::new(),
                created_at: 1,
            })
            .await
            .expect("create document");
        let chapter_id = store
            .create_chapter(&crate::knowledge::Chapter {
                id: 0,
                doc_id,
                chapter_no: 0,
                title: Some("第一回".into()),
                content: String::new(),
                start_offset: None,
                end_offset: None,
            })
            .await
            .expect("create chapter");
        let object_id = store
            .create_object(&KnowledgeObject {
                id: 0,
                doc_id,
                object_type: ObjectType::Person,
                name: "刘备".into(),
                properties: serde_json::json!({}),
                confidence: 1.0,
                created_at: 1,
            })
            .await
            .expect("create object");
        store
            .create_mention(&Mention {
                id: 0,
                object_id,
                chapter_id,
                start_offset: Some(5),
                end_offset: Some(7),
                alias_used: Some("玄德".into()),
                confidence: 0.9,
            })
            .await
            .expect("create mention");

        let exported = export_mentions(&store).await.expect("export");
        assert_eq!(exported.len(), 1, "the mention is exported");
        assert_eq!(exported[0].object_name, "刘备");
        assert_eq!(exported[0].start_offset, Some(5));
        assert_eq!(exported[0].alias_used.as_deref(), Some("玄德"));

        let dst = crate::knowledge::SQLiteKnowledgeStore::open_in_memory()
            .await
            .expect("open destination");
        let dst_doc = dst
            .create_document(&Document {
                id: 0,
                title: "三国".into(),
                author: None,
                doc_type: Some("text".into()),
                source: String::new(),
                created_at: 2,
            })
            .await
            .expect("create destination document");
        let dst_object = dst
            .create_object(&KnowledgeObject {
                id: 0,
                doc_id: dst_doc,
                object_type: ObjectType::Person,
                name: "刘备".into(),
                properties: serde_json::json!({}),
                confidence: 1.0,
                created_at: 2,
            })
            .await
            .expect("create destination object");
        let docs = HashMap::from([("三国".to_string(), dst_doc)]);
        let objects = HashMap::from([(("三国".to_string(), "刘备".to_string()), dst_object)]);

        let (created, skipped) = import_mentions(&dst, &exported, &docs, &objects)
            .await
            .expect("import");
        assert_eq!(
            (created, skipped),
            (1, 0),
            "the mention is created once, nothing is skipped"
        );
        let (again, skipped_again) = import_mentions(&dst, &exported, &docs, &objects)
            .await
            .expect("re-import");
        assert_eq!(
            (again, skipped_again),
            (0, 0),
            "the second import creates nothing"
        );
        let stored = dst
            .get_mentions_for_object(dst_object)
            .await
            .expect("read mentions");
        assert_eq!(stored.len(), 1, "exactly one mention row exists");
        assert_eq!(stored[0].start_offset, Some(5), "the span round-trips");
        assert_eq!(stored[0].alias_used.as_deref(), Some("玄德"));

        // A mention naming a document the bundle does not carry is skipped.
        let orphan = ExportMention {
            doc_title: "不存在的文档".into(),
            object_name: "刘备".into(),
            start_offset: Some(9),
            end_offset: Some(10),
            alias_used: None,
            confidence: 1.0,
        };
        let (created, skipped) = import_mentions(&dst, &[orphan], &docs, &objects)
            .await
            .expect("import an orphan mention");
        assert_eq!(
            (created, skipped),
            (0, 1),
            "an unknown document is skipped — and counted, so the loss is visible"
        );
    }
}
