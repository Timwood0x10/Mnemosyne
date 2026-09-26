//! Integration: document identity (`title`, `source`) and the legacy-schema
//! upgrade path — kept out of `store/tests/mod.rs` so that file stays under
//! the one-file-per-1000-lines rule (`plan/rules/rules.md` §1).

use mnemosyne::knowledge::Document;
use mnemosyne::knowledge::store::{KnowledgeStore, SQLiteKnowledgeStore};

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

async fn fresh() -> SQLiteKnowledgeStore {
    SQLiteKnowledgeStore::open_in_memory().await.expect("open")
}

/// Objective: Verify document identity is (title, source) — the same title
/// from two sources must stay two rows (the title-only merge bug), while
/// the title-only read path still resolves.
/// Invariants: two rows created; `find_document` hits exactly one per
/// (title, source) and misses on an unseen source; `source` round-trips;
/// `find_document_by_title` still finds the title.
#[tokio::test]
async fn document_identity_is_title_and_source() {
    let store = fresh().await;
    let mk = |title: &str, source: &str| Document {
        id: 0,
        title: title.into(),
        author: None,
        doc_type: Some("text".into()),
        source: source.into(),
        created_at: now_ts(),
    };
    let a = store
        .create_document(&mk("conversation", "source-a"))
        .await
        .expect("create a");
    let b = store
        .create_document(&mk("conversation", "source-b"))
        .await
        .expect("create b");
    assert_ne!(a, b, "same title, different sources → two documents");

    let got_a = store
        .find_document("conversation", "source-a")
        .await
        .expect("query a")
        .expect("row a");
    assert_eq!(got_a.id, a, "exact (title, source) hit");
    assert_eq!(got_a.source, "source-a", "source round-trips");

    let got_b = store
        .find_document("conversation", "source-b")
        .await
        .expect("query b")
        .expect("row b");
    assert_eq!(got_b.id, b, "second source resolves to its own row");

    assert!(
        store
            .find_document("conversation", "source-c")
            .await
            .expect("query c")
            .is_none(),
        "unseen source must miss"
    );

    let by_title = store
        .find_document_by_title("conversation")
        .await
        .expect("title query")
        .expect("title-only read path still works");
    assert!(
        by_title.id == a || by_title.id == b,
        "title lookup returns one of the titled rows"
    );
}

/// Objective: Verify opening a PRE-SOURCE `documents` table adds the column
/// via `ensure_column` (T12 migration path — `CREATE TABLE IF NOT EXISTS`
/// never alters an existing table).
/// Invariants: the legacy row reads back `source == ""`; the `(title, "")`
/// identity lookup adopts it; new writes on the upgraded DB carry source.
#[tokio::test]
async fn open_adds_source_column_to_legacy_documents() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("legacy.db");
    let path = path.to_str().expect("utf8 path");
    {
        let conn = rusqlite::Connection::open(path).expect("raw open");
        conn.execute_batch(
            "CREATE TABLE documents (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL,
                author TEXT,
                doc_type TEXT,
                created_at INTEGER
            );
            INSERT INTO documents (title, author, doc_type, created_at)
            VALUES ('旧文档', NULL, 'novel', 0);",
        )
        .expect("legacy schema without source");
    }

    let store = SQLiteKnowledgeStore::open(path).await.expect("open legacy");
    let doc = store
        .find_document_by_title("旧文档")
        .await
        .expect("query")
        .expect("legacy row readable after upgrade");
    assert_eq!(doc.source, "", "legacy rows backfill to the DEFAULT ''");
    assert!(
        store
            .find_document("旧文档", "")
            .await
            .expect("identity query")
            .is_some(),
        "(title, \"\") must adopt the legacy row"
    );

    let new_id = store
        .create_document(&Document {
            id: 0,
            title: "新文档".into(),
            author: None,
            doc_type: Some("text".into()),
            source: "origin-new".into(),
            created_at: now_ts(),
        })
        .await
        .expect("create on upgraded db");
    let new_doc = store
        .find_document("新文档", "origin-new")
        .await
        .expect("query")
        .expect("new row");
    assert_eq!(new_doc.id, new_id, "writes carry source after the upgrade");
}
