//! Unit tests for the fact store: schema upgrades, atomic writes, evidence
//! anchors and the identity repairs `initialize_schema` performs.
//!
//! They live in a sibling file instead of the end of `mod.rs` so neither file
//! crosses the one-file-per-1000-lines rule (`plan/rules/rules.md` §1) — the
//! same split `conversation_compiler/tests.rs` and `compiler/profile/tests.rs`
//! use. A child module keeps access to the private schema helpers.

use super::*;

fn sample_fact(entity_id: i64, fact_type: FactType) -> Fact {
    Fact {
        id: None,
        entity_id,
        fact_type,
        time: 2026,
        payload: serde_json::json!({"test": true}),
        evidence_id: None,
        created_at: 0,
        ..Fact::default()
    }
}

/// Objective: Verify single and batch writes preserve every fact.
/// Invariants: Every successful write returns an id/count and all rows are readable.
#[test]
fn writes_are_atomic_and_retrievable() {
    let store = SqliteFactStore::open_in_memory().expect("open fact store");
    let id = store
        .insert_fact(&sample_fact(100, FactType::Event))
        .expect("insert one fact");
    assert!(id > 0, "A successful insert must return a positive row id");

    let batch = vec![
        sample_fact(101, FactType::Goal),
        sample_fact(102, FactType::Preference),
    ];
    let count = store.insert_batch(&batch).expect("insert fact batch");
    assert_eq!(
        count, 2,
        "The transaction must commit every fact in the batch"
    );
    assert_eq!(
        store.get_facts(100).expect("read facts").len(),
        1,
        "The single inserted fact must remain readable"
    );
}

/// Objective: Verify fact filters and timelines preserve type and order.
/// Invariants: Type filtering excludes other facts and timeline is newest first.
#[test]
fn queries_preserve_type_and_timeline_order() {
    let store = SqliteFactStore::open_in_memory().expect("open fact store");
    for (year, fact_type) in [
        (2024, FactType::Event),
        (2025, FactType::Preference),
        (2026, FactType::Event),
    ] {
        let mut fact = sample_fact(7, fact_type);
        fact.time = year;
        store.insert_fact(&fact).expect("insert ordered fact");
    }

    let events = store
        .get_facts_by_type(7, FactType::Event)
        .expect("filter event facts");
    assert_eq!(
        events.len(),
        2,
        "Only Event facts should match the type filter"
    );
    let timeline = store.get_timeline(7).expect("read timeline");
    assert_eq!(
        timeline.len(),
        3,
        "The timeline must include every entity fact"
    );
    assert_eq!(timeline[0].time, 2026, "The newest fact must be first");
    assert_eq!(timeline[2].time, 2024, "The oldest fact must be last");
}

/// Objective: Verify the final cognition schema is complete and idempotent.
/// Invariants: Re-initialization preserves rows and all four core tables remain available.
#[test]
fn core_schema_is_complete_and_idempotent() {
    let store = SqliteFactStore::open_in_memory().expect("open fact store");
    {
        let conn = store.lock_conn().expect("lock fact database");
        SqliteFactStore::initialize_schema(&conn).expect("reinitialize cognition schema");
        for table in ["entities", "facts", "evidence", "aliases"] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .expect("query cognition table existence");
            assert_eq!(
                count, 1,
                "Core cognition table `{table}` must exist exactly once"
            );
        }
    }
}

/// Objective: Verify a legacy entities/evidence schema upgrades without data loss.
/// Invariants: Existing rows survive and tenant identity columns become queryable.
#[test]
fn legacy_schema_upgrade_preserves_existing_rows() {
    let conn = Connection::open_in_memory().expect("open legacy database");
    conn.execute_batch(
        "CREATE TABLE entities (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             name TEXT NOT NULL,
             entity_type TEXT NOT NULL DEFAULT 'person'
         );
         CREATE TABLE evidence (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             doc_id INTEGER NOT NULL,
             chapter_id INTEGER NOT NULL,
             content TEXT
         );
         INSERT INTO entities (name) VALUES ('Legacy Person');
         INSERT INTO evidence (doc_id, chapter_id, content) VALUES (1, 1, 'legacy');",
    )
    .expect("create legacy schema fixture");
    SqliteFactStore::initialize_schema(&conn).expect("upgrade legacy cognition schema");

    let identity: (String, String) = conn
        .query_row(
            "SELECT tenant_id, name FROM entities WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read upgraded legacy entity");
    assert_eq!(
        identity.0, "default",
        "Legacy entities must receive the default tenant"
    );
    assert_eq!(
        identity.1, "Legacy Person",
        "Schema migration must preserve entity names"
    );
    let evidence: String = conn
        .query_row("SELECT content FROM evidence WHERE id = 1", [], |row| {
            row.get(0)
        })
        .expect("read upgraded evidence row");
    assert_eq!(
        evidence, "legacy",
        "Schema migration must preserve evidence content"
    );
}

/// Objective: Verify malformed persisted rows surface typed errors.
/// Invariants: Unknown fact types and invalid JSON never degrade into Event/null facts.
#[test]
fn malformed_rows_return_invalid_data_errors() {
    let store = SqliteFactStore::open_in_memory().expect("open fact store");
    {
        let conn = store.lock_conn().expect("lock fact database");
        conn.execute(
            "INSERT INTO facts (entity_id, fact_type, time, payload, created_at) VALUES (1, 'unknown', 1, '{bad', 1)",
            [],
        )
        .expect("insert deliberately malformed row");
    }
    let error = store
        .get_facts(1)
        .expect_err("malformed rows must fail decoding");
    assert!(
        matches!(error, Error::Storage(StorageError::InvalidData(_))),
        "Malformed persisted data must return StorageError::InvalidData, got {error:?}"
    );
}

/// Objective: Verify the provenance columns (confidence/status/
/// derived_from) round-trip through insert → select on their own storage
/// rather than borrowing the legacy decay column.
/// Invariants: confidence is stored and read back exactly; status and
/// derived_from survive exactly; legacy rows default to Active with the
/// fact's default confidence.
#[test]
fn provenance_columns_roundtrip_and_legacy_rows_default() {
    let store = SqliteFactStore::open_in_memory().expect("open fact store");
    let id = store
        .insert_fact(&Fact {
            id: None,
            entity_id: 7,
            fact_type: FactType::Preference,
            time: 2026,
            payload: serde_json::json!({"content": "prefers Rust"}),
            evidence_id: Some(3),
            created_at: 2026,
            confidence: 0.85,
            derived_from: vec![4, 5],
            status: FactStatus::Contradicted,
        })
        .expect("insert fact with provenance fields");
    let facts = store.get_facts(7).expect("read facts back");
    assert_eq!(facts.len(), 1, "one fact inserted");
    let read = &facts[0];
    assert_eq!(read.id, Some(id), "id round-trips");
    assert_eq!(read.confidence, 0.85, "confidence stored and read back");
    assert_eq!(
        read.derived_from,
        vec![4, 5],
        "derived_from chain round-trips"
    );
    assert_eq!(read.status, FactStatus::Contradicted, "status round-trips");

    // A legacy row inserted without the new columns must decode as Active.
    {
        let conn = store.lock_conn().expect("lock fact database");
        conn.execute(
            "INSERT INTO facts (entity_id, fact_type, time, payload, created_at) VALUES (8, 'identity', 1, '{}', 1)",
            [],
        )
        .expect("insert legacy row without provenance columns");
    }
    let legacy = store.get_facts(8).expect("read legacy row");
    assert_eq!(legacy.len(), 1, "legacy row present");
    assert_eq!(
        legacy[0].status,
        FactStatus::Active,
        "legacy row defaults to Active"
    );
    assert_eq!(
        legacy[0].confidence, 1.0,
        "a row inserted without provenance columns defaults to full confidence"
    );
    assert!(
        legacy[0].derived_from.is_empty(),
        "legacy row defaults to empty derivation chain"
    );
}

/// Objective: Verify a database predating the provenance columns (a facts
/// table without the
/// status/derived_from columns) is migrated in place by ensure_column and
/// its existing rows survive with Active status.
/// Invariants: ensure_column adds exactly the missing columns; rows remain.
#[test]
fn legacy_schema_upgrades_in_place_without_data_loss() {
    let store = SqliteFactStore::open_in_memory().expect("open fact store");
    {
        let conn = store.lock_conn().expect("lock fact database");
        // Simulate a legacy schema: drop the provenance columns.
        conn.execute("ALTER TABLE facts DROP COLUMN status", [])
            .expect("drop status column to simulate legacy schema");
        conn.execute("ALTER TABLE facts DROP COLUMN derived_from", [])
            .expect("drop derived_from column to simulate legacy schema");
        conn.execute(
            "INSERT INTO facts (entity_id, fact_type, time, payload, created_at) VALUES (9, 'preference', 1, '{}', 1)",
            [],
        )
        .expect("insert row under legacy schema");
    }
    // Re-run schema init on the same database: it must re-add the columns.
    {
        let conn = store.lock_conn().expect("lock fact database");
        SqliteFactStore::initialize_schema(&conn)
            .expect("schema migration re-adds the provenance columns");
    }
    let facts = store.get_facts(9).expect("read migrated row");
    assert_eq!(facts.len(), 1, "row survives migration");
    assert_eq!(
        facts[0].status,
        FactStatus::Active,
        "migrated row defaults to Active"
    );
    assert!(
        facts[0].derived_from.is_empty(),
        "migrated row defaults to empty derivation chain"
    );
}

/// Objective: Verify decay no longer rewrites epistemic confidence — the
/// two used to share the `weight` column, so archiving a stale fact also
/// silently lowered "how much do we believe this?" (plan §2.3 forbids it).
/// Invariants: `set_decay` moves `weight`/`archived` only; the fact's
/// `confidence` and `status` are untouched.
#[test]
fn decay_does_not_change_confidence_or_status() {
    let store = SqliteFactStore::open_in_memory().expect("open fact store");
    let id = store
        .insert_fact(&Fact {
            id: None,
            entity_id: 7,
            fact_type: FactType::Event,
            time: 2026,
            payload: serde_json::json!({"content": "a low-confidence claim"}),
            evidence_id: None,
            created_at: 2026,
            confidence: 0.42,
            derived_from: Vec::new(),
            status: FactStatus::Active,
        })
        .expect("insert fact");
    assert_eq!(
        store.get_decay(id).expect("read decay state"),
        (1.0, false),
        "a freshly inserted fact carries no decay"
    );

    store.set_decay(id, 0.25, true).expect("archive the fact");

    assert_eq!(
        store.get_decay(id).expect("read decay state"),
        (0.25, true),
        "the decay state itself must be recorded"
    );
    let fact = store
        .get_fact_by_id(id)
        .expect("read fact back")
        .expect("the archived fact is never deleted");
    assert_eq!(
        fact.confidence, 0.42,
        "decay must not rewrite epistemic confidence"
    );
    assert_eq!(
        fact.status,
        FactStatus::Active,
        "decay must not rewrite the epistemic status"
    );
}

/// Objective: Verify a database created before the confidence column existed
/// migrates in place with its accumulated confidence preserved. The legacy
/// schema stored confidence in `weight`, so the backfill is what stops a
/// migration from silently resetting every fact to full confidence.
/// Invariants: after re-running schema init, `confidence` equals the legacy
/// `weight` value and the row survives.
#[test]
fn legacy_facts_backfill_confidence_from_weight() {
    let store = SqliteFactStore::open_in_memory().expect("open fact store");
    {
        let conn = store.lock_conn().expect("lock fact database");
        // Simulate the pre-split schema: no `confidence` column at all.
        conn.execute("ALTER TABLE facts DROP COLUMN confidence", [])
            .expect("drop confidence to simulate the legacy schema");
        conn.execute(
            "INSERT INTO facts (entity_id, fact_type, time, payload, created_at, weight)
             VALUES (9, 'preference', 1, '{\"content\":\"legacy\"}', 1, 0.4)",
            [],
        )
        .expect("insert legacy row carrying confidence in `weight`");
    }
    {
        let conn = store.lock_conn().expect("lock fact database");
        SqliteFactStore::initialize_schema(&conn)
            .expect("schema init re-adds and backfills the confidence column");
    }

    let facts = store.get_facts(9).expect("read migrated row");
    assert_eq!(facts.len(), 1, "the legacy row survives the migration");
    assert_eq!(
        facts[0].confidence, 0.4,
        "the legacy `weight` value becomes the fact's confidence"
    );
    let row_id = facts[0].id.expect("the legacy row exposes its id");
    assert_eq!(
        store.get_decay(row_id).expect("read decay state").0,
        0.4,
        "the legacy decay value stays readable after the split"
    );
}
/// Objective: Verify a database that accumulated duplicate anchors is
/// REPAIRED, not refused. The unique index cannot be created while
/// duplicates exist, so the installer has to collapse them first — and it
/// has to move `facts.evidence_id` onto the surviving row, because deleting
/// the duplicate outright would leave facts pointing at a row that is gone
/// (and, with foreign keys on, would be refused anyway).
/// Invariants: one row per identity survives, every fact points at it, and
/// a row with a different identity is untouched.
#[test]
fn duplicate_evidence_rows_are_collapsed_and_facts_repointed() {
    let conn = Connection::open_in_memory().expect("open database");
    SqliteFactStore::initialize_schema(&conn).expect("install schema");
    // Reproduce a database written before the index existed: drop it, then
    // create exactly the duplicates it would have prevented.
    conn.execute_batch("DROP INDEX ux_evidence_identity")
        .expect("drop the identity index");
    conn.execute_batch(
        "INSERT INTO entities (tenant_id, name) VALUES ('default', 'User');
         INSERT INTO evidence (id, tenant_id, start_offset, end_offset, content)
         VALUES (1, 'default', 10, 18, '重复的句子。'),
                (2, 'default', 10, 18, '重复的句子。'),
                (3, 'default', 300, 308, '重复的句子。');
         INSERT INTO facts (entity_id, fact_type, time, payload, evidence_id)
         VALUES (1, 'preference', 1, '{}', 1),
                (1, 'goal', 1, '{}', 2);",
    )
    .expect("create duplicates");

    SqliteFactStore::initialize_schema(&conn).expect("repair and install the index");

    let survivors: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT id FROM evidence ORDER BY id")
            .expect("prepare");
        stmt.query_map([], |row| row.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<i64>, _>>()
            .expect("collect")
    };
    assert_eq!(
        survivors,
        vec![2, 3],
        "the duplicate collapses onto the freshest row; the other span stays"
    );
    let anchors: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT evidence_id FROM facts ORDER BY id")
            .expect("prepare");
        stmt.query_map([], |row| row.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<i64>, _>>()
            .expect("collect")
    };
    assert_eq!(
        anchors,
        vec![2, 2],
        "both facts must follow their anchor onto the surviving row"
    );
}
