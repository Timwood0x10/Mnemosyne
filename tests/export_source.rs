//! Integration: document provenance (`documents.source`) through the
//! export/import round trip — kept out of `memory_export.rs` so that file
//! stays under the one-file-per-1000-lines rule (`plan/rules/rules.md` §1).

use mnemosyne::knowledge::KnowledgeStore;
use mnemosyne::knowledge::SQLiteKnowledgeStore;
use mnemosyne::knowledge::memory_export::{ExportDocument, export_store, import_bundle};

/// Objective: Verify `ExportDocument.source` travels both ways and that a
/// v1 document payload (no `source` key) deserializes to `""`.
/// Invariants: export echoes the row's source; import recreates the exact
/// (title, source) row; missing v1 key → `source == ""`; the title-only
/// read path still resolves the imported document.
#[tokio::test]
async fn document_source_round_trips_and_v1_defaults() {
    // v1 bundle compatibility: documents written before the column have no
    // `source` key — serde default fills "".
    let v1: ExportDocument = serde_json::from_value(serde_json::json!({
        "title": "legacy",
        "author": null,
        "doc_type": null,
    }))
    .expect("v1 document payload must deserialize");
    assert_eq!(v1.source, "", "missing source key defaults to empty");

    let src = SQLiteKnowledgeStore::open_in_memory().await.expect("src");
    src.create_document(&mnemosyne::knowledge::Document {
        id: 0,
        title: "doc-a".into(),
        author: None,
        doc_type: Some("text".into()),
        source: "origin-a".into(),
        created_at: chrono::Utc::now().timestamp(),
    })
    .await
    .expect("create");

    let exported = export_store(&src).await.expect("export");
    assert_eq!(
        exported.documents[0].source, "origin-a",
        "export must echo the provenance tag"
    );

    let dst = SQLiteKnowledgeStore::open_in_memory().await.expect("dst");
    import_bundle(&dst, &exported).await.expect("import");
    let got = dst
        .find_document("doc-a", "origin-a")
        .await
        .expect("query")
        .expect("exact (title, source) row after import");
    assert_eq!(got.source, "origin-a", "source survives the round trip");
    assert!(
        dst.find_document_by_title("doc-a")
            .await
            .expect("title query")
            .is_some(),
        "title-only read path still resolves the imported document"
    );
}
