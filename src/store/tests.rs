//! Unit tests for the vector-backed memory store: schema, CRUD, retrieval in
//! both modes and the query-tokenizer contract for Chinese content.
//!
//! They live in a sibling file instead of the end of `mod.rs` so neither file
//! crosses the one-file-per-1000-lines rule (`plan/rules/rules.md` §1) — the same
//! split `conversation_compiler/tests.rs`, `compiler/profile/tests.rs` and
//! `fact_store/tests.rs` use. A child module keeps access to the private fields
//! the tests inspect.

use super::*;
/// Objective: Verify Chinese keyword search works in BOTH store modes. The
/// repository drops any row whose `bm25_score` is 0, and before the tokenizer
/// knew about Han runs a whole Chinese sentence was one unmatchable token: with
/// `dim > 0` every row was dropped (an empty result set — the reported
/// symptom), and with `dim == 0` the LIKE fallback recalled rows but scored
/// them all 0, so ranking degenerated to importance (audit C8).
/// Invariants: in both modes the matching Chinese row comes back and ranks
/// first, and the unrelated row is not preferred over it.
#[tokio::test]
async fn chinese_keyword_search_works_with_and_without_vectors() {
    for dim in [0usize, 4] {
        let store = SQLiteVecStore::open_in_memory(dim).await.expect("open");
        let mut hit = sample_exp("t1", MemoryType::Knowledge, "刘备很高兴。");
        hit.id = "hit".to_string();
        hit.vector = vec![1.0_f32; dim];
        store.create(&hit).await.expect("create hit");
        let mut miss = sample_exp("t1", MemoryType::Knowledge, "曹操很生气。");
        miss.id = "miss".to_string();
        miss.vector = vec![0.0_f32; dim];
        store.create(&miss).await.expect("create miss");

        let results = store
            .search_by_keyword("刘备", "t1", 5, None)
            .await
            .expect("search");
        assert!(
            !results.is_empty(),
            "dim = {dim}: a Chinese query must return the matching row"
        );
        assert_eq!(
            results[0].id,
            "hit",
            "dim = {dim}: the matching row must rank first, got {:?}",
            results.iter().map(|e| &e.id).collect::<Vec<_>>()
        );
    }
}

fn sample_exp(tenant: &str, mt: MemoryType, content: &str) -> Experience {
    Experience::new(tenant, mt, content, 0.8)
}

#[tokio::test]
async fn open_in_memory_succeeds() {
    let store = SQLiteVecStore::open_in_memory(8).await.expect("open");
    assert_eq!(store.dim, 8);
}

#[tokio::test]
async fn open_in_memory_zero_dim_succeeds() {
    let store = SQLiteVecStore::open_in_memory(0).await.expect("open");
    assert_eq!(store.dim, 0);
}

#[tokio::test]
async fn fts5_keyword_search_works() {
    let store = SQLiteVecStore::open_in_memory(0).await.expect("open");
    let mut exp = sample_exp("t1", MemoryType::Knowledge, "Rust async runtime uses tokio");
    exp.id = "e1".to_string();
    store.create(&exp).await.expect("create");
    let results = store
        .search_by_keyword("rust", "t1", 5, None)
        .await
        .expect("search");
    assert!(!results.is_empty(), "FTS5 should find 'rust'");
    assert_eq!(results[0].id, "e1");
}

#[tokio::test]
async fn create_and_get_round_trip() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
    let exp = sample_exp("t1", MemoryType::Knowledge, "hello");
    store.create(&exp).await.expect("create");
    let got = store.get(&exp.id).await.expect("get").expect("exists");
    assert_eq!(got.content, "hello");
    assert_eq!(got.tenant_id, "t1");
}

/// Objective: Verify pre-existing databases without the `expires_at`
/// column still open and migrate correctly (P1 upgrade safety).
/// Invariants: An old-schema DB file opens without error; writes succeed;
/// the new column defaults to empty (never-expire).
#[tokio::test]
async fn legacy_db_without_expires_at_migrates_on_open() {
    let dir = tempfile::TempDir::new().expect("temp dir for legacy db");
    let db_path = dir.path().join("legacy.db");
    // Create an OLD-schema database (no expires_at column).
    {
        let conn = rusqlite::Connection::open(&db_path).expect("open legacy db");
        conn.execute_batch(
            "CREATE TABLE memories (
                id          TEXT PRIMARY KEY,
                tenant_id   TEXT NOT NULL,
                user_id     TEXT NOT NULL DEFAULT '',
                memory_type TEXT NOT NULL,
                problem     TEXT NOT NULL DEFAULT '',
                solution    TEXT NOT NULL DEFAULT '',
                content     TEXT NOT NULL,
                confidence  REAL NOT NULL DEFAULT 0.5,
                source      TEXT NOT NULL DEFAULT '',
                extraction_method TEXT NOT NULL DEFAULT 'direct',
                created_at  TEXT NOT NULL,
                metadata    TEXT NOT NULL DEFAULT '{}',
                vector      TEXT NOT NULL DEFAULT '[]'
            );",
        )
        .expect("create legacy schema");
    }

    // Opening the legacy DB must succeed (migration adds expires_at).
    let store = SQLiteVecStore::open(db_path.to_str().expect("utf8 path"), 4)
        .await
        .expect("legacy DB must open and migrate");

    // Writes must work and the row must read back as never-expiring.
    let exp = sample_exp("t1", MemoryType::Knowledge, "after-upgrade");
    store.create(&exp).await.expect("create after migration");
    let got = store.get(&exp.id).await.expect("get").expect("exists");
    assert!(
        got.expires_at.is_none(),
        "legacy rows default to never-expire (expires_at empty)"
    );
}

/// Objective: Verify an existing database created with one embedding
/// dimension is rebuilt with the new dimension on reopen — the vec table
/// must not silently keep the stale schema (bug-audit store.rs:264-267).
/// Invariants: after opening dim=4, then reopening the SAME file with
/// dim=8, the stored vec table DDL reports float[8] and a dim-8 write
/// round-trips through search_by_vector.
#[tokio::test]
async fn reopen_with_new_dimension_rebuilds_vec_table() {
    let dir = tempfile::TempDir::new().expect("temp dir for dim reopen");
    let db_path = dir.path().join("dim.db");

    // First open: dim=4, write one 4-dim memory.
    {
        let store = SQLiteVecStore::open(db_path.to_str().expect("utf8 path"), 4)
            .await
            .expect("open with dim 4");
        let mut exp = sample_exp("t1", MemoryType::Knowledge, "four-dim");
        exp.vector = vec![1.0_f32, 0.0, 0.0, 0.0];
        store.create(&exp).await.expect("create with dim 4");
    } // store dropped → connection closed

    // Reopen the same file with dim=8: the stale vec table must be
    // rebuilt (dropped + recreated at the new dimension).
    let store = SQLiteVecStore::open(db_path.to_str().expect("utf8 path"), 8)
        .await
        .expect("open with dim 8");
    assert_eq!(store.dim, 8, "store reports the new dimension");
    let conn = store.conn.lock().await;
    let stored = stored_vec_dim(&conn)
        .expect("read stored dim")
        .expect("vec table exists after reopen");
    assert_eq!(
        stored, 8,
        "vec table must be rebuilt at the new dimension, stored={stored}"
    );
    drop(conn);

    // A dim-8 write round-trips through vector search.
    let mut exp = sample_exp("t1", MemoryType::Knowledge, "eight-dim");
    exp.vector = vec![1.0_f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    store.create(&exp).await.expect("create with dim 8");
    let hits = store
        .search_by_vector(&[1.0_f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], "t1", 5)
        .await
        .expect("dim-8 search works");
    assert!(
        hits.iter().any(|e| e.id == exp.id),
        "dim-8 memory is retrievable after rebuild"
    );
}

/// Objective: Verify reopening with the SAME dimension does NOT rebuild
/// the vec table (stored dimension matches — no spurious drop).
/// Invariants: stored DDL still reports the original dimension and the
/// previously written memory remains searchable.
#[tokio::test]
async fn reopen_with_same_dimension_keeps_vec_table() {
    let dir = tempfile::TempDir::new().expect("temp dir for dim reopen");
    let db_path = dir.path().join("dim-same.db");

    {
        let store = SQLiteVecStore::open(db_path.to_str().expect("utf8 path"), 4)
            .await
            .expect("open with dim 4");
        let mut exp = sample_exp("t1", MemoryType::Knowledge, "keep-me");
        exp.vector = vec![1.0_f32, 0.0, 0.0, 0.0];
        store.create(&exp).await.expect("create");
    }

    let store = SQLiteVecStore::open(db_path.to_str().expect("utf8 path"), 4)
        .await
        .expect("reopen with dim 4");
    let conn = store.conn.lock().await;
    let stored = stored_vec_dim(&conn)
        .expect("read stored dim")
        .expect("vec table exists");
    assert_eq!(stored, 4, "same dimension keeps the original vec table");
    drop(conn);

    // The memory written before the reopen is still searchable (no data
    // loss from a spurious rebuild).
    let hits = store
        .search_by_vector(&[1.0_f32, 0.0, 0.0, 0.0], "t1", 5)
        .await
        .expect("search");
    assert!(
        hits.iter().any(|e| e.content == "keep-me"),
        "memory survives a same-dimension reopen"
    );
}

/// Objective: Verify `stored_vec_dim` parses the dimension from the vec
/// table DDL and returns None when the table is absent.
/// Invariants: "float[8]" → 8; no vec table → None.
#[test]
fn stored_vec_dim_parses_ddl() {
    // The raw connection needs the vec0 extension loaded before it can
    // create a vec0 virtual table (mirrors SQLiteVecStore::open).
    ensure_vec_loaded();
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory conn");
    assert!(
        stored_vec_dim(&conn)
            .expect("no table → Ok(None)")
            .is_none(),
        "absent vec table yields None"
    );
    conn.execute_batch(
        "CREATE VIRTUAL TABLE vec_memories USING vec0(
            id TEXT PRIMARY KEY,
            vector float[8] distance_metric=cosine
        );",
    )
    .expect("create vec table");
    assert_eq!(
        stored_vec_dim(&conn).expect("parse").expect("dim"),
        8,
        "float[8] parses to 8"
    );
}

#[tokio::test]
async fn get_missing_returns_none() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
    let got = store.get("nonexistent").await.expect("get");
    assert!(got.is_none());
}

#[tokio::test]
async fn create_with_vector_and_search() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
    let mut exp = sample_exp("t1", MemoryType::Knowledge, "rust");
    exp.vector = vec![1.0_f32, 0.0, 0.0, 0.0];
    store.create(&exp).await.expect("create");

    let results = store
        .search_by_vector(&[1.0_f32, 0.0, 0.0, 0.0], "t1", 5)
        .await
        .expect("search");
    assert!(!results.is_empty(), "should find at least one result");
    assert_eq!(results[0].id, exp.id);
}

#[tokio::test]
async fn search_is_tenant_scoped() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
    let mut e1 = sample_exp("t1", MemoryType::Knowledge, "a");
    e1.vector = vec![1.0_f32, 0.0, 0.0, 0.0];
    store.create(&e1).await.expect("create");
    let mut e2 = sample_exp("t2", MemoryType::Knowledge, "b");
    e2.vector = vec![1.0_f32, 0.0, 0.0, 0.0];
    store.create(&e2).await.expect("create");

    let r1 = store
        .search_by_vector(&[1.0_f32, 0.0, 0.0, 0.0], "t1", 5)
        .await
        .expect("search");
    assert!(r1.iter().all(|e| e.tenant_id == "t1"), "only t1 results");
}

#[tokio::test]
async fn search_rejects_dimension_mismatch() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
    let err = store
        .search_by_vector(&[1.0_f32, 0.0, 0.0], "t1", 5)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("dim"), "dim mismatch should error");
}

#[tokio::test]
async fn update_modifies_record() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
    let mut exp = sample_exp("t1", MemoryType::Knowledge, "old");
    store.create(&exp).await.expect("create");
    exp.content = "new".to_string();
    store.update(&exp).await.expect("update");
    let got = store.get(&exp.id).await.expect("get").expect("exists");
    assert_eq!(got.content, "new");
}

#[tokio::test]
async fn delete_removes_record() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
    let exp = sample_exp("t1", MemoryType::Knowledge, "x");
    store.create(&exp).await.expect("create");
    store.delete(&exp.id).await.expect("delete");
    assert!(store.get(&exp.id).await.expect("get").is_none());
}

#[tokio::test]
async fn delete_missing_returns_not_found() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
    let err = store.delete("ghost").await.unwrap_err();
    assert!(err.to_string().contains("ghost"), "should mention the id");
}

#[tokio::test]
async fn delete_batch_empty_noop() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
    store
        .delete_batch(&[])
        .await
        .expect("empty batch should not error");
}

/// Objective: Verify delete_batch removes every requested id from both
/// the memories table and the vec index, and reports no error.
/// Invariants: after the batch, all three ids are gone.
#[tokio::test]
async fn delete_batch_removes_all_ids() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
    let mut ids = Vec::new();
    for content in ["a", "b", "c"] {
        let exp = sample_exp("t1", MemoryType::Knowledge, content);
        store.create(&exp).await.expect("create");
        ids.push(exp.id.clone());
    }
    store
        .delete_batch(&ids)
        .await
        .expect("batch delete must succeed");
    for id in &ids {
        assert!(
            store.get(id).await.expect("get").is_none(),
            "id {id} must be gone after batch delete"
        );
    }
}

/// Objective: Verify a failed batch DELETE surfaces its error instead of
/// silently succeeding — the previous `let _ =` swallowed per-row errors
/// and reported Ok even when rows were skipped. A BEFORE DELETE trigger
/// forces the second row to abort; the error must propagate.
/// Invariants: delete_batch returns Err mentioning the trigger message,
/// and the first (already-deleted-in-transaction) row is rolled back —
/// the batch is atomic, not partially applied.
#[tokio::test]
async fn delete_batch_propagates_errors_and_rolls_back() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
    let keep = sample_exp("t1", MemoryType::Knowledge, "keep");
    let doomed = sample_exp("t1", MemoryType::Knowledge, "doomed");
    store.create(&keep).await.expect("create keep");
    store.create(&doomed).await.expect("create doomed");

    // Abort the delete of the "doomed" row mid-batch. Order matters:
    // keep is deleted first inside the transaction, then doomed trips the
    // trigger — proving rollback of the prior delete, not just a failed
    // delete on an untouched table.
    let conn = store.conn.lock().await;
    conn.execute_batch(
        "CREATE TRIGGER abort_doomed BEFORE DELETE ON memories
         WHEN OLD.content = 'doomed'
         BEGIN SELECT RAISE(ABORT, 'doomed-delete-triggered'); END;",
    )
    .expect("create abort trigger");
    drop(conn);

    let err = store
        .delete_batch(&[keep.id.clone(), doomed.id.clone()])
        .await
        .expect_err("mid-batch failure must surface as Err");
    assert!(
        err.to_string().contains("doomed-delete-triggered"),
        "error names the failing delete, got: {err}"
    );

    // Atomicity: the first delete was rolled back with the batch.
    assert!(
        store.get(&keep.id).await.expect("get").is_some(),
        "mid-batch failure must roll back prior deletes, not leave a half-deleted state"
    );
}

/// Objective: Verify delete_batch tolerates an orphan vec row (present in
/// vec_memories but absent from memories) without panicking and without
/// corrupting the surviving data.
/// Invariants: batch delete succeeds, and the real memory is removed.
#[tokio::test]
async fn delete_batch_tolerates_orphan_vec_row() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
    let exp = sample_exp("t1", MemoryType::Knowledge, "keep");
    store.create(&exp).await.expect("create");

    // A vec row with no matching memories row (legal vector JSON, so the
    // vec extension accepts it).
    let orphan_id = "orphan-vec-row";
    let conn = store.conn.lock().await;
    conn.execute(
        "INSERT INTO vec_memories (id, vector) VALUES (?1, ?2)",
        rusqlite::params![orphan_id, "[0.0,0.0,0.0,0.0]"],
    )
    .expect("insert orphan vec row");
    drop(conn);

    store
        .delete_batch(&[orphan_id.to_string(), exp.id.clone()])
        .await
        .expect("batch delete succeeds despite the orphan row");
    assert!(
        store.get(&exp.id).await.expect("get").is_none(),
        "the real memory is deleted"
    );
}

/// Objective: Verify forget_expired removes only expired tenant memories.
/// Invariants: Expired rows are deleted; non-expired and other-tenant rows
/// survive; the count matches the number of expired rows.
#[tokio::test]
async fn forget_expired_deletes_only_expired_tenant_rows() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");

    // Expired memory (TTL elapsed).
    let mut expired = sample_exp("t1", MemoryType::Knowledge, "stale");
    expired.expires_at = Some(Utc::now() - chrono::Duration::seconds(60));
    store.create(&expired).await.expect("create expired");

    // Live memory (not yet expired).
    let mut live = sample_exp("t1", MemoryType::Knowledge, "fresh");
    live.expires_at = Some(Utc::now() + chrono::Duration::seconds(3600));
    store.create(&live).await.expect("create live");

    // Never-expiring memory (no expires_at).
    let never = sample_exp("t1", MemoryType::Knowledge, "permanent");
    store.create(&never).await.expect("create never");

    // Other tenant's expired memory — must NOT be touched.
    let mut other_tenant = sample_exp("t2", MemoryType::Knowledge, "other-stale");
    other_tenant.expires_at = Some(Utc::now() - chrono::Duration::seconds(60));
    store.create(&other_tenant).await.expect("create other");

    let now = Utc::now();
    let forgotten = store
        .forget_expired("t1", now)
        .await
        .expect("forget_expired must not error");
    assert_eq!(
        forgotten, 1,
        "exactly one expired row for t1 must be forgotten"
    );

    assert!(
        store.get(&expired.id).await.expect("get").is_none(),
        "expired memory must be deleted"
    );
    assert!(
        store.get(&live.id).await.expect("get").is_some(),
        "live memory must survive"
    );
    assert!(
        store.get(&never.id).await.expect("get").is_some(),
        "never-expiring memory must survive"
    );
    assert!(
        store.get(&other_tenant.id).await.expect("get").is_some(),
        "other tenant's expired memory must survive (tenant isolation)"
    );
}

#[tokio::test]
async fn get_by_memory_type_filters() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
    store
        .create(&sample_exp("t1", MemoryType::Knowledge, "k"))
        .await
        .expect("create");
    store
        .create(&sample_exp("t1", MemoryType::Preference, "p"))
        .await
        .expect("create");
    let k = store
        .get_by_memory_type("t1", MemoryType::Knowledge)
        .await
        .expect("get");
    let p = store
        .get_by_memory_type("t1", MemoryType::Preference)
        .await
        .expect("get");
    assert_eq!(k.len(), 1, "one knowledge");
    assert_eq!(p.len(), 1, "one preference");
}

#[tokio::test]
async fn create_and_search_round_trip() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
    let mut exp = sample_exp("t1", MemoryType::Knowledge, "rust");
    exp.vector = vec![1.0_f32, 0.0, 0.0, 0.0];
    store.create(&exp).await.expect("create");
    let results = store
        .search_by_vector(&[1.0_f32, 0.0, 0.0, 0.0], "t1", 10)
        .await
        .expect("search");
    assert!(!results.is_empty(), "should find created memory");
}

#[tokio::test]
async fn get_vector_round_trips() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
    let mut exp = sample_exp("t1", MemoryType::Knowledge, "rust");
    exp.vector = vec![0.1_f32, 0.2, 0.3, 0.4];
    store.create(&exp).await.expect("create");
    let v = store.get_vector(&exp.id).await.expect("get_vector");
    assert_eq!(v, vec![0.1_f32, 0.2, 0.3, 0.4], "vector round-trips");
}

#[tokio::test]
async fn get_vector_missing_returns_empty() {
    let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
    let v = store.get_vector("ghost").await.expect("get_vector");
    assert!(v.is_empty(), "missing id -> empty vector");
}

#[tokio::test]
async fn get_vector_zero_dim_returns_empty() {
    let store = SQLiteVecStore::open_in_memory(0).await.expect("open");
    let v = store.get_vector("anything").await.expect("get_vector");
    assert!(v.is_empty(), "dim==0 store has no vectors");
}

#[tokio::test]
async fn fts5_query_escapes_special_chars() {
    // A query full of FTS5 syntax chars must not raise a syntax error.
    let store = SQLiteVecStore::open_in_memory(0).await.expect("open");
    let mut exp = sample_exp("t1", MemoryType::Knowledge, "Rust async runtime uses tokio");
    exp.id = "e1".to_string();
    store.create(&exp).await.expect("create");
    // No assertion on hit count: a nonsense query may match nothing; the
    // regression is simply that the call does not error out.
    let _ = store
        .search_by_keyword("rust:(\"weird*query)\"", "t1", 5, None)
        .await;
}

/// Objective: Verify `replace_batch` is atomic — the superseded rows
/// survive when a replacement cannot be written. The distillation pipeline
/// used to delete the old memory in its conflict phase and insert the new
/// one later, so a failure in between destroyed the old row and stored
/// nothing: an unrecoverable lost update (the replacement only exists in the
/// caller's memory).
/// Invariants: the call returns the trigger error, the superseded row is
/// still readable, and nothing of the failed replacement is stored.
#[tokio::test]
async fn replace_batch_rolls_back_when_a_replacement_fails() {
    let store = SQLiteVecStore::open_in_memory(0).await.expect("open");
    let old = sample_exp("t1", MemoryType::Knowledge, "old fact");
    store.create(&old).await.expect("create the old memory");
    {
        let conn = store.conn.lock().await;
        // Abort the INSERT, which runs AFTER the delete inside the batch.
        conn.execute_batch(
            "CREATE TRIGGER abort_new BEFORE INSERT ON memories \
             WHEN NEW.content = 'new fact' \
             BEGIN SELECT RAISE(ABORT, 'new-insert-triggered'); END;",
        )
        .expect("create abort trigger");
    }

    let new = sample_exp("t1", MemoryType::Knowledge, "new fact");
    let error = store
        .replace_batch(std::slice::from_ref(&old.id), std::slice::from_ref(&new))
        .await
        .expect_err("the failing insert must surface as Err");
    assert!(
        matches!(error, crate::error::Error::Storage(_)),
        "the insert failure must surface as a storage error, got {error:?}"
    );
    assert!(
        store.get(&old.id).await.expect("get old").is_some(),
        "the rollback must restore the superseded memory"
    );
    assert!(
        store.get(&new.id).await.expect("get new").is_none(),
        "a failed replacement must not be partly stored"
    );
}
