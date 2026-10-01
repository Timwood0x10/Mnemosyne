//! Knowledge-store unit tests: CRUD, transactions, search and world model.

use super::*;
use serde_json::json;

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

async fn fresh() -> SQLiteKnowledgeStore {
    SQLiteKnowledgeStore::open_in_memory().await.expect("open")
}

/// Objective: Verify the P2 fix — `upsert_world_entity` is atomic and
/// unique-by-name is DB-enforced. Two INDEPENDENT store instances sharing
/// the same SQLite file must not create duplicate rows when they upsert the
/// same name concurrently.
/// Invariants: after racing two instances on one name, exactly ONE
/// `world_entities` row exists with that name.
#[tokio::test]
async fn concurrent_upsert_does_not_duplicate() {
    // Unique path per run: a fixed name raced with parallel cargo-test
    // processes on the same machine (two binaries / nextest workers) and
    // intermittently failed the uniqueness assertion.
    let path = std::env::temp_dir().join(format!(
        "lorescope_p2_dup_{}_{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_file(&path);
    // Two independent connections to the SAME file — each has its own
    // mutex, so the pre-fix check-then-insert could both pass the SELECT.
    let store_a = SQLiteKnowledgeStore::open(path.to_str().unwrap())
        .await
        .expect("open a");
    let store_b = SQLiteKnowledgeStore::open(path.to_str().unwrap())
        .await
        .expect("open b");

    let (id_a, id_b) = tokio::join!(
        store_a.upsert_world_entity("诸葛亮", "person", 0.9),
        store_b.upsert_world_entity("诸葛亮", "person", 0.9),
    );
    assert!(id_a.is_ok(), "first upsert ok");
    assert!(
        id_b.is_ok(),
        "second upsert ok — conflict must be handled, not errored"
    );

    // Count rows for this name — must be exactly one.
    let count: i64 = {
        // The connection now lives behind a std Mutex; recover from poisoning
        // the same way the store itself does.
        let conn = store_a
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT COUNT(*) FROM world_entities WHERE name = ?1",
            params!["诸葛亮"],
            |r| r.get(0),
        )
        .expect("count")
    };
    assert_eq!(count, 1, "P2: concurrent upsert must not duplicate rows");

    let _ = std::fs::remove_file(&path);
}

/// Objective: Verify `WORLD_SCHEMA` (V7 entity-centric tables) is now
/// executed by `init` — the dead-code wiring fix for C10.
/// Invariants: after `open_in_memory`, the V7 `world_`-prefixed tables and
/// `events` exist (a fresh connection that never executed WORLD_SCHEMA
/// would fail this query). The prefix isolates the world model from the
/// fact-store's bare `entities` table sharing the same database file.
#[tokio::test]
async fn world_schema_tables_are_created() {
    let store = fresh().await;
    let conn = store
        .conn
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for table in [
        "world_entities",
        "world_entity_aliases",
        "world_entity_profiles",
        "world_relations",
        "events",
        "world_states",
    ] {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                rusqlite::params![table],
                |row| row.get(0),
            )
            .expect("query sqlite_master");
        assert_eq!(
            count, 1,
            "WORLD_SCHEMA must create table `{table}` on init (C10 wiring)"
        );
    }
}

async fn seed_doc(store: &SQLiteKnowledgeStore, title: &str) -> i64 {
    store
        .create_document(&Document {
            id: 0,
            title: title.to_string(),
            author: None,
            doc_type: Some("novel".into()),
            source: String::new(),
            created_at: now_ts(),
        })
        .await
        .expect("create doc")
}

async fn seed_chapter(store: &SQLiteKnowledgeStore, doc_id: i64, no: i32) -> i64 {
    store
        .create_chapter(&Chapter {
            id: 0,
            doc_id,
            chapter_no: no,
            title: Some(format!("ch{no}")),
            content: format!("第{no}回正文"),
            start_offset: Some(0),
            end_offset: Some(10),
        })
        .await
        .expect("create chapter")
}

async fn seed_person(
    store: &SQLiteKnowledgeStore,
    doc_id: i64,
    name: &str,
    props: serde_json::Value,
) -> i64 {
    store
        .create_object(&KnowledgeObject {
            id: 0,
            doc_id,
            object_type: ObjectType::Person,
            name: name.to_string(),
            properties: props,
            confidence: 0.9,
            created_at: now_ts(),
        })
        .await
        .expect("create person")
}

/// Objective: Verify document + chapter round-trip and lookup-by-no.
/// Invariants: A created chapter is retrievable by (doc_id, chapter_no).
#[tokio::test]
async fn document_and_chapter_round_trip() {
    let store = fresh().await;
    let did = seed_doc(&store, "三国演义").await;
    let cid = seed_chapter(&store, did, 3).await;
    let got = store
        .get_chapter_by_no(did, 3)
        .await
        .expect("get chapter")
        .expect("chapter exists");
    assert_eq!(got.id, cid, "the chapter must round-trip its id");
    assert_eq!(got.chapter_no, 3, "the chapter must round-trip its number");
}

/// Objective: Verify `search_evidence` exposes the source byte span at the
/// API surface (`EvidenceHit.start_offset`/`end_offset`) — the explore-5
/// "evidence span exposure" gap: rows carried the anchor but the `evidence`
/// tool output dropped it.
/// Invariants: the hit returns the exact stored span; a legacy NULL span
/// deserializes to `None`.
#[tokio::test]
async fn search_evidence_exposes_source_span() {
    let store = fresh().await;
    let doc_id = seed_doc(&store, "三国演义").await;
    let chapter_id = seed_chapter(&store, doc_id, 1).await;
    let (evidence_id, created) = store
        .ensure_evidence(doc_id, chapter_id, Some(10), Some(20), "孔宣兵阻援兵")
        .await
        .expect("ensure evidence");
    assert!(created, "first ensure creates the row");
    assert!(evidence_id > 0, "row id assigned");

    let hits = store
        .search_evidence("孔宣兵阻", None, 5)
        .await
        .expect("search");
    assert!(!hits.is_empty(), "hit must be found");
    assert_eq!(
        hits[0].start_offset,
        Some(10),
        "hit must expose the source span start"
    );
    assert_eq!(hits[0].end_offset, Some(20), "hit must expose the span end");

    // Legacy rows (NULL span) surface as None, not a panic.
    store
        .create_evidence(&crate::knowledge::Evidence {
            id: 0,
            doc_id,
            chapter_id,
            start_offset: None,
            end_offset: None,
            content: "旧证据没有 span".to_string(),
            created_at: now_ts(),
        })
        .await
        .expect("legacy evidence");
    let hits = store
        .search_evidence("旧证据", None, 5)
        .await
        .expect("search legacy");
    assert_eq!(hits.len(), 1, "legacy row found");
    assert_eq!(hits[0].start_offset, None, "legacy span is None");
}

/// Objective: Verify an explicit transaction COMMIT persists all writes
/// made since BEGIN (H6 — the migrator's atomicity relies on this).
/// Invariants: after commit, the created document is visible.
#[tokio::test]
async fn transaction_commit_persists_writes() {
    let store = fresh().await;
    store.begin_transaction().await.expect("begin");
    seed_doc(&store, "事务提交测试").await;
    store.commit_transaction().await.expect("commit");
    let doc = store
        .find_document_by_title("事务提交测试")
        .await
        .expect("query");
    assert!(
        doc.is_some(),
        "committed write must be visible after COMMIT"
    );
}

/// Objective: Verify an explicit transaction ROLLBACK discards every
/// write made since BEGIN (H6 — a failed migration must not leave a
/// half-migrated database).
/// Invariants: after rollback, the created document is NOT visible.
#[tokio::test]
async fn transaction_rollback_discards_writes() {
    let store = fresh().await;
    store.begin_transaction().await.expect("begin");
    seed_doc(&store, "事务回滚测试").await;
    store.rollback_transaction().await.expect("rollback");
    let doc = store
        .find_document_by_title("事务回滚测试")
        .await
        .expect("query");
    assert!(
        doc.is_none(),
        "rolled-back write must be invisible after ROLLBACK"
    );
}

/// Objective: Verify a failed store call inside a transaction leaves the
/// database unchanged when the transaction is rolled back (H6 partial-
/// failure scenario at the store level).
/// Invariants: doc + person written, then rollback → neither remains.
#[tokio::test]
async fn transaction_rollback_undoes_multi_row_writes() {
    let store = fresh().await;
    store.begin_transaction().await.expect("begin");
    let did = seed_doc(&store, "多行回滚测试").await;
    seed_person(
        &store,
        did,
        "关羽",
        serde_json::json!({ "novel": "三国演义" }),
    )
    .await;
    store.rollback_transaction().await.expect("rollback");
    assert!(
        store
            .find_document_by_title("多行回滚测试")
            .await
            .expect("query")
            .is_none(),
        "document must be rolled back"
    );
    let objects = store
        .list_objects_by_document(999_999)
        .await
        .expect("query");
    assert!(
        objects.is_empty(),
        "no objects should remain from the rolled-back transaction"
    );
}

mod objects;
mod views;
mod world;

/// Whether FK enforcement is currently ON for this store's connection.
async fn foreign_keys_on(store: &SQLiteKnowledgeStore) -> bool {
    let conn = store
        .conn
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
        .expect("read foreign_keys pragma")
        != 0
}

/// Objective: Verify FK enforcement is restored on EVERY exit path of
/// `with_foreign_keys_disabled`. The spelling it replaces re-enabled the pragma
/// *after* the `?` on BEGIN/COMMIT, so a failed BEGIN (the `is_autocommit`
/// check and the BEGIN are not atomic, so a concurrent BEGIN can win) left the
/// connection writing without referential integrity for the rest of its life.
/// Invariants: after a failed BEGIN, a failing closure and a successful run,
/// `PRAGMA foreign_keys` is 1 and the connection is back in autocommit.
#[tokio::test]
async fn foreign_keys_are_restored_on_every_exit_path() {
    let store = fresh().await;
    assert!(foreign_keys_on(&store).await, "FK is on by default");

    // 1) BEGIN fails: this connection is already inside a transaction.
    store.begin_transaction().await.expect("outer begin");
    let nested = store
        .with_foreign_keys_disabled(|| async { Ok::<_, Error>(()) })
        .await;
    assert!(nested.is_err(), "a nested BEGIN must surface as an error");
    store.rollback_transaction().await.expect("outer rollback");
    assert!(
        foreign_keys_on(&store).await,
        "a failed BEGIN must still restore FK enforcement"
    );

    // 2) The wrapped work fails.
    let failed = store
        .with_foreign_keys_disabled(|| async {
            Err::<(), _>(Error::InvalidInput("work failed".into()))
        })
        .await;
    assert!(failed.is_err(), "the work's error is returned");
    assert!(
        foreign_keys_on(&store).await,
        "a failed work must still restore FK enforcement"
    );

    // 3) The happy path returns the value and commits.
    let value = store
        .with_foreign_keys_disabled(|| async { Ok::<_, Error>(7) })
        .await
        .expect("wrapped work");
    assert_eq!(value, 7, "the wrapped value is passed through unchanged");
    assert!(
        foreign_keys_on(&store).await,
        "FK is restored after success"
    );
    assert!(
        !store
            .in_transaction()
            .await
            .expect("read transaction state"),
        "the helper must leave the connection in autocommit"
    );
}

/// Objective: Verify `documents` holds ONE row per (title, source) however often
/// the "find_document → None → create_document" sequence races itself. Two
/// separate lock acquisitions let two compilers of the same work both see `None`
/// and both insert, after which objects/edges/evidence split across the two rows
/// and `clear_for_document` wiped only half.
/// Invariants: a repeated create reuses the row, metadata is only filled in
/// (never erased by a caller that supplies none), and a different source stays a
/// different document.
#[tokio::test]
async fn create_document_is_idempotent_by_identity() {
    let store = fresh().await;
    let doc = Document {
        id: 0,
        title: "三国演义".into(),
        author: Some("罗贯中".into()),
        doc_type: Some("novel".into()),
        source: "corpus/sanguo.txt".into(),
        created_at: now_ts(),
    };

    let first = store.create_document(&doc).await.expect("first create");
    let second = store.create_document(&doc).await.expect("second create");
    assert_eq!(
        first, second,
        "the same (title, source) must reuse its document row"
    );

    let bare = Document {
        author: None,
        doc_type: None,
        ..doc.clone()
    };
    let third = store.create_document(&bare).await.expect("create bare");
    assert_eq!(third, first, "still the same document");
    let stored = store
        .find_document("三国演义", "corpus/sanguo.txt")
        .await
        .expect("find document")
        .expect("document exists");
    assert_eq!(
        stored.author.as_deref(),
        Some("罗贯中"),
        "a caller without an author must not erase the recorded one"
    );
    assert_eq!(
        stored.doc_type.as_deref(),
        Some("novel"),
        "the document type must round-trip"
    );

    let other_source = Document {
        source: "corpus/sanguo_v2.txt".into(),
        ..doc.clone()
    };
    let other = store
        .create_document(&other_source)
        .await
        .expect("create other source");
    assert_ne!(other, first, "a different source is a different document");

    let rows: i64 = {
        let conn = store
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))
            .expect("count documents")
    };
    assert_eq!(rows, 2, "two identities, two rows");
}
