//! Integration tests for the unique-index migration of a **dirty** database.
//!
//! `init` installs two unique expression indexes that make the world-model
//! upserts atomic (`ux_events_identity`, `ux_world_states_identity`). Databases
//! written before those indexes can contain exactly the duplicates they exist to
//! prevent — and the revision that introduced them could not repair such a
//! database: the dedupe `DELETE` was refused by the foreign keys `init` turns on
//! (`FOREIGN KEY constraint failed`), after which the `CREATE UNIQUE INDEX`
//! failed too, so `open()` returned an error on every attempt and the service
//! never started.
//!
//! These cases exist because that failure mode is invisible on a fresh database:
//! every other store test opens an empty in-memory store, where the dedupe is a
//! no-op. They build the dirty state with `rusqlite` directly, then assert that
//! opening the store repairs it instead of refusing it.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use mnemosyne::knowledge::store::{KnowledgeStore, SQLiteKnowledgeStore};
use mnemosyne::storage::{KNOWLEDGE_SCHEMA, WORLD_SCHEMA};

/// Create a file-backed database with both schemas applied and foreign keys ON
/// (the state `init` leaves the connection in), seeded by `seed`.
///
/// Both schemas are applied because the world tables reference the general-model
/// ones (`world_entity_profiles.evidence_id` → `evidence`), and because the
/// unique indexes this file is about are created by `init`, never by the DDL —
/// which is exactly the legacy state these tests start from.
///
/// The connection is closed before returning so the store opens it fresh.
fn seeded_database(dir: &tempfile::TempDir, seed: impl FnOnce(&Connection)) -> PathBuf {
    let path = dir.path().join("world.db");
    let conn = Connection::open(&path).expect("open raw database");
    conn.execute_batch("PRAGMA foreign_keys = ON")
        .expect("fk on");
    conn.execute_batch(KNOWLEDGE_SCHEMA)
        .expect("apply knowledge schema");
    conn.execute_batch(WORLD_SCHEMA)
        .expect("apply world schema");
    seed(&conn);
    drop(conn);
    path
}

/// Insert an event with the identity used throughout these tests.
fn insert_event(conn: &Connection, title: &str, start: i64, end: i64) -> i64 {
    conn.execute(
        "INSERT INTO events (title, event_type, timestamp, start_offset, end_offset) \
         VALUES (?1, 'action', 3, ?2, ?3)",
        rusqlite::params![title, start, end],
    )
    .expect("insert event");
    conn.last_insert_rowid()
}

/// Insert a world entity and return its id.
fn insert_entity(conn: &Connection, name: &str) -> i64 {
    conn.execute(
        "INSERT INTO world_entities (name) VALUES (?1)",
        rusqlite::params![name],
    )
    .expect("insert entity");
    conn.last_insert_rowid()
}

/// Link `entity_name` to `event_id` as a participant.
fn link_participant(conn: &Connection, event_id: i64, entity_id: i64, role: &str) {
    conn.execute(
        "INSERT INTO event_participants (event_id, entity_id, role) VALUES (?1, ?2, ?3)",
        rusqlite::params![event_id, entity_id, role],
    )
    .expect("link participant");
}

/// Read the stored SQL of an index, if it exists.
fn index_sql(path: &Path, name: &str) -> Option<String> {
    let conn = Connection::open(path).expect("open database");
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
        rusqlite::params![name],
        |row| row.get(0),
    )
    .ok()
}

/// Objective: Verify a database carrying the duplicates the index exists to
/// prevent is REPAIRED on open, not refused. The previous revision's dedupe was
/// blocked by the foreign keys (participants and states reference the duplicate
/// about to be deleted), so the following `CREATE UNIQUE INDEX` failed and
/// `open()` failed with it — leaving the service unable to start at all.
/// Invariants: `open` succeeds; the identity group collapses to the freshest row
/// (`MAX(id)`); every dependent row points at that survivor; the unique index
/// exists afterwards.
#[tokio::test]
async fn dirty_database_is_repaired_instead_of_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = seeded_database(&dir, |conn| {
        let doomed = insert_event(conn, "刘备 死", 10, 12);
        let survivor = insert_event(conn, "刘备 死", 10, 12);
        assert!(doomed < survivor, "the second insert must be the freshest");
        let liubei = insert_entity(conn, "刘备");
        link_participant(conn, doomed, liubei, "subject");
        conn.execute(
            "INSERT INTO world_states (entity_id, slot, value, event_id, chapter) \
             VALUES (?1, 'status', 'deceased', ?2, 3)",
            rusqlite::params![liubei, doomed],
        )
        .expect("insert world state");
    });

    let store = SQLiteKnowledgeStore::open(path.to_str().expect("utf-8 path"))
        .await
        .expect("a dirty database must be repaired, not refused");

    let events = store.list_world_events().await.expect("list events");
    assert_eq!(
        events.len(),
        1,
        "the identity group must collapse to one row, got {events:?}"
    );
    assert_eq!(
        events[0].id, 2,
        "the freshest row is kept (MAX id), matching DO UPDATE semantics"
    );

    let states = store.list_world_states(None).await.expect("list states");
    assert_eq!(states.len(), 1, "the state row survives the repair");
    assert_eq!(
        states[0].event_id,
        Some(2),
        "the state must be re-pointed at the surviving event"
    );

    let conn = Connection::open(&path).expect("reopen database");
    let participants: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT event_id FROM event_participants")
            .expect("prepare");
        let rows = stmt
            .query_map([], |row| row.get(0))
            .expect("query participants");
        rows.map(|row| row.expect("row")).collect()
    };
    assert_eq!(
        participants,
        vec![2],
        "the participant must be re-pointed, never orphaned"
    );
    assert!(
        index_sql(&path, "ux_events_identity").is_some(),
        "the repair must end with the unique index installed"
    );
}

/// Objective: Verify the repair also handles dependents that would COLLIDE when
/// re-pointed: two duplicates that each link the same participant. A plain
/// `UPDATE` would violate `UNIQUE(event_id, entity_id)` and fail the repair
/// (which would brick the database again); the link the survivor already has
/// must simply win.
/// Invariants: `open` succeeds; exactly one participant row remains, pointing at
/// the survivor; a participant that exists only on the doomed event is kept.
#[tokio::test]
async fn colliding_dependents_are_merged_onto_the_survivor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = seeded_database(&dir, |conn| {
        let doomed = insert_event(conn, "吕布 杀 董卓", 100, 112);
        let survivor = insert_event(conn, "吕布 杀 董卓", 100, 112);
        let lvbu = insert_entity(conn, "吕布");
        let dongzhuo = insert_entity(conn, "董卓");
        // The same participant on both rows: re-pointing must not duplicate it.
        link_participant(conn, doomed, lvbu, "subject");
        link_participant(conn, survivor, lvbu, "subject");
        // …and one that only the doomed row knows about: it must survive.
        link_participant(conn, doomed, dongzhuo, "object");
    });

    SQLiteKnowledgeStore::open(path.to_str().expect("utf-8 path"))
        .await
        .expect("the colliding dependent must be merged, not fail the repair");

    let conn = Connection::open(&path).expect("reopen database");
    let mut stmt = conn
        .prepare("SELECT event_id, entity_id, role FROM event_participants ORDER BY entity_id")
        .expect("prepare");
    let rows: Vec<(i64, i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query")
        .map(|row| row.expect("row"))
        .collect();
    assert_eq!(
        rows,
        vec![(2, 1, "subject".to_string()), (2, 2, "object".to_string())],
        "both links live on the survivor, and the duplicate link is not doubled"
    );
}

/// Objective: Verify an index that carries the right NAME but a different
/// definition is rebuilt. Comparing index names alone accepts a stale index, and
/// then every `ON CONFLICT (...)` upsert fails at statement level with a message
/// that hides the cause.
/// Invariants: `open` succeeds; the stored definition is the identity one.
#[tokio::test]
async fn stale_index_definition_is_rebuilt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = seeded_database(&dir, |conn| {
        // Same name, wrong columns — what an earlier revision could have left.
        conn.execute_batch("CREATE UNIQUE INDEX ux_events_identity ON events(event_type)")
            .expect("create stale index");
        insert_event(conn, "刘备 死", 10, 12);
    });

    SQLiteKnowledgeStore::open(path.to_str().expect("utf-8 path"))
        .await
        .expect("a stale index must be rebuilt, not accepted");

    let sql = index_sql(&path, "ux_events_identity").expect("index exists");
    let normalized = sql.to_ascii_lowercase();
    assert!(
        normalized.contains("ifnull(timestamp"),
        "the rebuilt index must use the identity expressions, got `{sql}`"
    );
    assert!(
        normalized.contains("ifnull(start_offset"),
        "the rebuilt index must cover the source span, got `{sql}`"
    );
}

/// Objective: Verify a row whose OPTIONAL columns are NULL stays readable.
/// `events.description`, `events/world_entities.importance` and the three
/// `confidence` columns are nullable in the DDL but were read as non-null, so a
/// single legacy or hand-written row made every `list_world_*` call — and with
/// it the whole export path — fail until someone deleted the row by hand.
/// Invariants: every list returns the row, with the value its DDL default would
/// have supplied filled in.
#[tokio::test]
async fn null_optional_columns_do_not_break_reads() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = seeded_database(&dir, |conn| {
        conn.execute(
            "INSERT INTO world_entities (name, entity_type, importance) \
             VALUES ('刘备', 'person', NULL)",
            [],
        )
        .expect("entity with NULL importance");
        conn.execute(
            "INSERT INTO world_entity_profiles (entity_id, key, value, confidence) \
             VALUES (1, 'status', 'active', NULL)",
            [],
        )
        .expect("profile with NULL confidence");
        conn.execute(
            "INSERT INTO world_relations (source_id, target_id, relation_type, confidence) \
             VALUES (1, 1, 'knows', NULL)",
            [],
        )
        .expect("relation with NULL confidence");
        conn.execute(
            "INSERT INTO events (title, event_type, description, importance, timestamp) \
             VALUES ('刘备 死', 'action', NULL, NULL, 3)",
            [],
        )
        .expect("event with NULL description and importance");
        conn.execute(
            "INSERT INTO world_states (entity_id, slot, value, confidence) \
             VALUES (1, 'status', 'deceased', NULL)",
            [],
        )
        .expect("state with NULL confidence");
    });

    let store = SQLiteKnowledgeStore::open(path.to_str().expect("utf-8 path"))
        .await
        .expect("legacy NULLs must not stop the store from opening");

    let entities = store.list_world_entities().await.expect("entities");
    assert_eq!(
        entities[0].importance, 0.5,
        "a NULL importance reads as the DDL default"
    );
    let profiles = store.list_world_profiles().await.expect("profiles");
    assert_eq!(
        profiles[0].confidence, 1.0,
        "a NULL profile confidence reads as its DDL default"
    );
    let relations = store.list_world_relations().await.expect("relations");
    assert_eq!(
        relations[0].confidence, 1.0,
        "a NULL relation confidence reads as its DDL default"
    );
    let events = store.list_world_events().await.expect("events");
    assert_eq!(
        events[0].description, "",
        "a NULL description reads as empty text, not an error"
    );
    assert_eq!(
        events[0].importance, 0.5,
        "a NULL event importance reads as the DDL default"
    );
    let states = store.list_world_states(None).await.expect("states");
    assert_eq!(
        states[0].confidence, 0.8,
        "a NULL state confidence reads as its (lower) DDL default"
    );
}

/// Objective: Verify a clean database is untouched: the repair path must not
/// invent work (or delete rows) when there is nothing to collapse, and opening
/// twice must stay idempotent.
/// Invariants: rows survive both opens; the index exists; no duplicates reported.
#[tokio::test]
async fn clean_database_is_left_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = seeded_database(&dir, |conn| {
        insert_event(conn, "刘备 死", 10, 12);
        insert_event(conn, "华雄 斩 某", 200, 210);
    });

    for _ in 0..2 {
        let store = SQLiteKnowledgeStore::open(path.to_str().expect("utf-8 path"))
            .await
            .expect("open");
        assert_eq!(
            store.list_world_events().await.expect("list").len(),
            2,
            "a clean database must keep all of its rows"
        );
    }
    assert!(
        index_sql(&path, "ux_events_identity").is_some(),
        "the index must exist after opening"
    );
}

/// Objective: Verify a database holding two `documents` rows with the same
/// `(title, source)` is REPAIRED on open. That is what a binary predating the
/// identity index produced whenever two compilers raced the
/// "find → None → create" sequence, and the unique index cannot be created while
/// the duplicates exist.
/// Invariants: `open` succeeds; one row survives per identity; every child
/// (chapter, object, evidence, compiler run) points at the survivor; the same
/// title under another source stays a separate document; the index exists.
#[tokio::test]
async fn duplicate_documents_are_collapsed_and_children_repointed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = seeded_database(&dir, |conn| {
        conn.execute_batch(
            "INSERT INTO documents (id, title, source) VALUES
                 (1, '三国演义', 'corpus/sanguo.txt'),
                 (2, '三国演义', 'corpus/sanguo.txt'),
                 (3, '三国演义', 'corpus/other.txt');
             INSERT INTO chapters (doc_id, chapter_no, title) VALUES
                 (1, 1, '第一回'), (2, 1, '第一回');
             INSERT INTO knowledge_objects (doc_id, object_type, name) VALUES
                 (1, 'person', '刘备'), (2, 'person', '关羽');
             INSERT INTO evidence (doc_id, content) VALUES
                 (1, '锚点'), (2, '另一个锚点');
             INSERT INTO compiler_runs (doc_id, version) VALUES
                 (1, 'v1'), (2, 'v1');",
        )
        .expect("seed duplicate documents");
    });

    let store = SQLiteKnowledgeStore::open(path.to_str().expect("utf-8 path"))
        .await
        .expect("a database with duplicate documents must be repaired, not refused");

    let conn = Connection::open(&path).expect("reopen database");
    let survivors: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT id FROM documents ORDER BY id")
            .expect("prepare");
        stmt.query_map([], |row| row.get(0))
            .expect("query")
            .collect::<Result<Vec<i64>, _>>()
            .expect("collect")
    };
    assert_eq!(
        survivors,
        vec![2, 3],
        "the duplicate collapses onto the freshest row; the other source stays"
    );

    for (table, column) in [
        ("chapters", "doc_id"),
        ("knowledge_objects", "doc_id"),
        ("evidence", "doc_id"),
        ("compiler_runs", "doc_id"),
    ] {
        let stale: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE {column} = 1"),
                [],
                |row| row.get(0),
            )
            .expect("count stale children");
        assert_eq!(
            stale, 0,
            "{table}.{column} must be re-pointed off the removed duplicate"
        );
    }

    let found = store
        .find_document("三国演义", "corpus/sanguo.txt")
        .await
        .expect("find document")
        .expect("document exists");
    assert_eq!(found.id, 2, "reads follow the surviving row");
    assert!(
        index_sql(&path, "ux_documents_identity").is_some(),
        "the identity index must be installed by the repair"
    );
}

/// Objective: Verify the DDL's timestamp defaults actually produce a value. The
/// default was `strftime('%s','localtime')`, and `localtime` is a *modifier*, so
/// passing it where SQLite expects a time value makes the whole expression
/// evaluate to NULL — every row inserted without an explicit timestamp stored
/// NULL instead of a time.
/// Invariants: a row inserted without a timestamp gets the current UTC second.
#[tokio::test]
async fn ddl_timestamp_defaults_are_not_null() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = seeded_database(&dir, |conn| {
        conn.execute(
            "INSERT INTO documents (title, source) VALUES ('默认值', 'corpus/default.txt')",
            [],
        )
        .expect("insert without a timestamp");
    });

    let conn = Connection::open(&path).expect("reopen database");
    let created_at: Option<i64> = conn
        .query_row(
            "SELECT created_at FROM documents WHERE title = '默认值'",
            [],
            |row| row.get(0),
        )
        .expect("read created_at");
    let created_at = created_at.expect(
        "the DDL default must produce a timestamp, not NULL \
         (`strftime('%s','localtime')` evaluates to NULL)",
    );
    let now = chrono::Utc::now().timestamp();
    assert!(
        (created_at - now).abs() < 60,
        "the default must be the current UTC second, got {created_at} vs {now}"
    );
}

/// Objective: Verify rows whose `created_at` is NULL stay READABLE. NULL is what
/// every row inserted without an explicit timestamp carried while the DDL
/// default was broken, and the four row mappers decode `created_at` as a plain
/// `i64` — so a single NULL made a document (and everything reached through it)
/// unreadable, permanently.
/// Invariants: each list reader returns the row with `created_at == 0` ("time
/// unknown") instead of an error.
#[tokio::test]
async fn null_timestamps_do_not_break_reads() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = seeded_database(&dir, |conn| {
        conn.execute_batch(
            "INSERT INTO documents (id, title, source, created_at) \
                 VALUES (1, '三国演义', 'corpus/sanguo.txt', NULL);
             INSERT INTO knowledge_objects (id, doc_id, object_type, name, created_at) \
                 VALUES (1, 1, 'person', '刘备', NULL);
             INSERT INTO knowledge_edges (source_id, target_id, predicate, created_at) \
                 VALUES (1, 1, 'knows', NULL);
             INSERT INTO evidence (doc_id, content, created_at) VALUES (1, '锚点', NULL);",
        )
        .expect("seed NULL timestamps");
    });

    let store = SQLiteKnowledgeStore::open(path.to_str().expect("utf-8 path"))
        .await
        .expect("open store");

    let documents = store.list_documents().await.expect("list documents");
    assert_eq!(documents.len(), 1);
    assert_eq!(
        documents[0].created_at, 0,
        "an unknown document time must read as 0, not fail the row"
    );

    let objects = store
        .list_objects_by_document(1)
        .await
        .expect("list objects");
    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].created_at, 0);

    let edges = store.list_edges_by_document(1).await.expect("list edges");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].created_at, 0);

    let evidence = store
        .list_evidence_by_document(1)
        .await
        .expect("list evidence");
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].created_at, 0);
}
