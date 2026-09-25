//! FactStore — SQLite persistence layer for immutable cognition facts.
//!
//! The implementation uses explicit transactions, strict decoding, and the
//! crate-wide storage error type. No database or serialization failure is
//! converted into a successful-looking zero or empty result.

use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};

use crate::cognition::{Fact, FactStatus, FactStore, FactType};
use crate::error::{Error, Result, StorageError};

mod decisions;
mod entities;
mod evidence;
mod relationship;

const CORE_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS entities (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    tenant_id    TEXT NOT NULL DEFAULT 'default',
    external_key TEXT,
    name         TEXT NOT NULL,
    entity_type  TEXT NOT NULL DEFAULT 'person',
    status       TEXT NOT NULL DEFAULT 'active',
    importance   REAL DEFAULT 0.5,
    created_at   INTEGER DEFAULT (strftime('%s','now')),
    updated_at   INTEGER DEFAULT (strftime('%s','now'))
);
CREATE TABLE IF NOT EXISTS facts (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id    INTEGER NOT NULL,
    fact_type    TEXT NOT NULL,
    time         INTEGER NOT NULL,
    payload      TEXT NOT NULL,
    evidence_id  INTEGER,
    created_at   INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    confidence   REAL NOT NULL DEFAULT 1.0,
    weight       REAL DEFAULT 1.0,
    archived     INTEGER DEFAULT 0,
    status       TEXT NOT NULL DEFAULT 'active',
    derived_from TEXT NOT NULL DEFAULT '[]'
);
CREATE TABLE IF NOT EXISTS evidence (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    tenant_id    TEXT NOT NULL DEFAULT 'default',
    doc_id       INTEGER,
    chapter_id   INTEGER,
    start_offset INTEGER,
    end_offset   INTEGER,
    content      TEXT,
    created_at   INTEGER DEFAULT (strftime('%s','now'))
);
CREATE TABLE IF NOT EXISTS aliases (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id   INTEGER NOT NULL,
    alias       TEXT NOT NULL,
    alias_type  TEXT NOT NULL DEFAULT 'known_as',
    confidence  REAL DEFAULT 1.0,
    UNIQUE(entity_id, alias)
);
CREATE INDEX IF NOT EXISTS idx_facts_entity ON facts(entity_id);
CREATE INDEX IF NOT EXISTS idx_facts_type ON facts(entity_id, fact_type);
CREATE INDEX IF NOT EXISTS idx_facts_time ON facts(entity_id, time);
CREATE INDEX IF NOT EXISTS idx_aliases_core_entity ON aliases(entity_id);
CREATE INDEX IF NOT EXISTS idx_aliases_core_alias ON aliases(alias);
CREATE TABLE IF NOT EXISTS relationship_state (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    tenant_id       TEXT NOT NULL DEFAULT 'default',
    agent_entity_id INTEGER NOT NULL,
    user_entity_id  INTEGER NOT NULL,
    intimacy        REAL NOT NULL DEFAULT 0.0,
    stage           TEXT NOT NULL DEFAULT 'stranger',
    emotion_trend   TEXT NOT NULL DEFAULT 'stable',
    recent_topics   TEXT NOT NULL DEFAULT '[]',
    updated_at      INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    UNIQUE(tenant_id, agent_entity_id, user_entity_id)
);
CREATE INDEX IF NOT EXISTS idx_relationship_user ON relationship_state(tenant_id, user_entity_id);
CREATE TABLE IF NOT EXISTS decisions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    subject     INTEGER NOT NULL,
    verb        TEXT NOT NULL,
    object      TEXT NOT NULL,
    made_at     INTEGER NOT NULL,
    because     TEXT NOT NULL DEFAULT '[]',
    outcome     TEXT,
    status      TEXT NOT NULL DEFAULT 'open',
    created_at  INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE INDEX IF NOT EXISTS idx_decisions_subject ON decisions(subject);
CREATE INDEX IF NOT EXISTS idx_decisions_made_at ON decisions(subject, made_at);
";

/// Columns every fact read selects, in the order [`SqliteFactStore::row_to_fact`]
/// expects. Shared so a new column can never be added to one query only.
const FACT_COLUMNS: &str = "id, entity_id, fact_type, time, payload, evidence_id, created_at,
                    confidence, status, derived_from";

/// The single fact insert every write path uses.
const INSERT_FACT_SQL: &str =
    "INSERT INTO facts (entity_id, fact_type, time, payload, evidence_id, created_at,
                            confidence, status, derived_from)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";

/// SQLite-backed fact store.
pub struct SqliteFactStore {
    conn: Mutex<Connection>,
}

impl SqliteFactStore {
    /// Open or create a fact store at `path`.
    ///
    /// # Errors
    ///
    /// Returns a storage error if SQLite cannot open or initialize the schema.
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        // busy_timeout: several connections share one DB file; without it a
        // concurrent writer gets SQLITE_BUSY immediately (0 ms default).
        conn.execute_batch("PRAGMA busy_timeout = 5000;")?;
        Self::initialize_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an isolated in-memory fact store.
    ///
    /// # Errors
    ///
    /// Returns a storage error if SQLite cannot initialize the schema.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA busy_timeout = 5000;")?;
        Self::initialize_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Initialize the four-table cognition schema and upgrade compatible legacy tables.
    fn initialize_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(CORE_SCHEMA)?;
        Self::ensure_column(
            conn,
            "entities",
            "tenant_id",
            "TEXT NOT NULL DEFAULT 'default'",
        )?;
        Self::ensure_column(conn, "entities", "external_key", "TEXT")?;
        Self::ensure_column(
            conn,
            "evidence",
            "tenant_id",
            "TEXT NOT NULL DEFAULT 'default'",
        )?;
        // Decay columns: down-weighting only ever writes these flags and never
        // deletes the row, so the persona evolution timeline stays intact.
        Self::ensure_column(conn, "facts", "weight", "REAL DEFAULT 1.0")?;
        Self::ensure_column(conn, "facts", "archived", "INTEGER DEFAULT 0")?;
        // Epistemic confidence owns its OWN column. Historically `weight`
        // doubled as confidence because decay was its only writer, so decaying
        // a fact silently rewrote "how much do we believe this?" — exactly what
        // the plan forbids (§2.3: confidence ≠ status ≠ decay). When the column
        // is created for the first time it is backfilled from `weight` so a
        // legacy database keeps the confidence it had accumulated.
        if Self::ensure_column(conn, "facts", "confidence", "REAL NOT NULL DEFAULT 1.0")? {
            // COALESCE, not a bare copy: `weight` is a nullable legacy column,
            // and a single NULL row would make this UPDATE violate the new
            // NOT NULL constraint — failing `open()` outright instead of
            // degrading, so the whole store would refuse to start.
            conn.execute("UPDATE facts SET confidence = COALESCE(weight, 1.0)", [])?;
        }
        // Cognitive-state columns: epistemic status (legacy `archived`/
        // `weight` stay; `status`/`derived_from` are additive).
        Self::ensure_column(conn, "facts", "status", "TEXT NOT NULL DEFAULT 'active'")?;
        Self::ensure_column(conn, "facts", "derived_from", "TEXT NOT NULL DEFAULT '[]'")?;
        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_entities_tenant_external
                 ON entities(tenant_id, external_key) WHERE external_key IS NOT NULL;
             CREATE INDEX IF NOT EXISTS idx_entities_tenant_name
                 ON entities(tenant_id, name);",
        )?;
        Ok(())
    }

    /// Add `column` to `table` when it is missing.
    ///
    /// Returns `true` when this call created the column, so callers can run a
    /// one-time backfill for rows that predate it.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the schema cannot be inspected or altered.
    fn ensure_column(
        conn: &Connection,
        table: &str,
        column: &str,
        definition: &str,
    ) -> Result<bool> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            if row.get::<_, String>(1)? == column {
                return Ok(false);
            }
        }
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
        Ok(true)
    }

    fn lock_conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|error| Error::Storage(StorageError::LockPoisoned(error.to_string())))
    }

    fn parse_fact_type(value: &str) -> Result<FactType> {
        match value {
            "identity" => Ok(FactType::Identity),
            "preference" => Ok(FactType::Preference),
            "goal" => Ok(FactType::Goal),
            "event" => Ok(FactType::Event),
            "relationship" => Ok(FactType::Relationship),
            "emotion" => Ok(FactType::Emotion),
            "location" => Ok(FactType::Location),
            "occupation" => Ok(FactType::Occupation),
            "interest" => Ok(FactType::Interest),
            "habit" => Ok(FactType::Habit),
            other => Err(Error::Storage(StorageError::InvalidData(format!(
                "unknown fact type `{other}`"
            )))),
        }
    }

    fn row_to_fact(row: &rusqlite::Row<'_>) -> Result<Fact> {
        let fact_type = Self::parse_fact_type(&row.get::<_, String>("fact_type")?)?;
        let payload_text: String = row.get("payload")?;
        let payload = serde_json::from_str(&payload_text).map_err(|error| {
            Error::Storage(StorageError::InvalidData(format!(
                "fact payload is not valid JSON: {error}"
            )))
        })?;
        let derived_from_text: String = row.get("derived_from")?;
        let derived_from: Vec<i64> = serde_json::from_str(&derived_from_text).map_err(|error| {
            Error::Storage(StorageError::InvalidData(format!(
                "fact derived_from is not a valid id array: {error}"
            )))
        })?;
        Ok(Fact {
            id: Some(row.get("id")?),
            entity_id: row.get("entity_id")?,
            fact_type,
            time: row.get("time")?,
            payload,
            evidence_id: row.get("evidence_id")?,
            created_at: row.get("created_at")?,
            // Epistemic confidence is its own column and is NEVER written by
            // the decay path, so `factor in decay` and `how much do we believe
            // this?` stay independent (plan §2.3).
            confidence: row.get("confidence")?,
            derived_from,
            status: FactStatus::parse(&row.get::<_, String>("status")?),
        })
    }

    /// Bind every column of `fact` into a prepared [`INSERT_FACT_SQL`] and
    /// execute it, storing `evidence_id` as the fact's anchor.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the payload cannot be serialized or the
    /// insert fails.
    fn bind_fact(
        stmt: &mut rusqlite::Statement<'_>,
        fact: &Fact,
        evidence_id: Option<i64>,
    ) -> Result<()> {
        let payload = serde_json::to_string(&fact.payload)?;
        let derived_from = serde_json::to_string(&fact.derived_from)?;
        stmt.execute(params![
            fact.entity_id,
            fact.fact_type.as_str(),
            fact.time,
            payload,
            evidence_id,
            fact.created_at,
            fact.confidence,
            fact.status.as_str(),
            derived_from,
        ])?;
        Ok(())
    }

    /// Insert `facts` on an already-locked connection, registering the
    /// original-text evidence anchor each fact carries.
    ///
    /// `anchors` caches the rows created by this write, so the several facts
    /// compiled from one utterance share a single evidence row instead of one
    /// row each.
    ///
    /// # Errors
    ///
    /// Returns a storage error when an insert fails.
    fn insert_facts_on(
        conn: &Connection,
        facts: &[Fact],
        anchors: &mut std::collections::HashMap<String, i64>,
    ) -> Result<()> {
        let mut stmt = conn.prepare(INSERT_FACT_SQL)?;
        for fact in facts {
            let evidence_id = match fact.evidence_id {
                Some(id) => Some(id),
                None => Self::anchor_evidence_on(conn, &fact.payload, anchors)?,
            };
            Self::bind_fact(&mut stmt, fact, evidence_id)?;
        }
        Ok(())
    }

    /// Read every fact of an entity on an already-locked connection.
    ///
    /// The transactional compile path calls this so the facts it just wrote are
    /// visible to the commitment scan inside the same transaction.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the read or the decode fails.
    fn get_facts_on(conn: &Connection, entity_id: i64) -> Result<Vec<Fact>> {
        let mut stmt = conn.prepare(&format!(
            "SELECT {FACT_COLUMNS} FROM facts WHERE entity_id = ?1 ORDER BY time, created_at, id"
        ))?;
        let mut rows = stmt.query(params![entity_id])?;
        let mut facts = Vec::new();
        while let Some(row) = rows.next()? {
            facts.push(Self::row_to_fact(row)?);
        }
        Ok(facts)
    }

    fn read_facts(
        &self,
        sql: &str,
        entity_id: i64,
        fact_type: Option<FactType>,
    ) -> Result<Vec<Fact>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(sql)?;
        let mut rows = match fact_type {
            Some(value) => stmt.query(params![entity_id, value.as_str()])?,
            None => stmt.query(params![entity_id])?,
        };
        let mut facts = Vec::new();
        while let Some(row) = rows.next()? {
            facts.push(Self::row_to_fact(row)?);
        }
        Ok(facts)
    }

    /// Write back a decay score and archive flag for a fact.
    ///
    /// This is the **only** decay write path and it never deletes the row — the
    /// fact stays readable so the persona evolution timeline remains
    /// reconstructable. It deliberately leaves `confidence` and `status`
    /// untouched: decay, epistemic confidence and lifecycle status are
    /// orthogonal (plan §2.3).
    ///
    /// # Errors
    ///
    /// Returns a storage error when the update fails.
    pub fn set_decay(&self, fact_id: i64, score: f64, archived: bool) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE facts SET weight = ?1, archived = ?2 WHERE id = ?3",
            params![score, i64::from(archived), fact_id],
        )?;
        Ok(())
    }

    /// List the archived (down-weighted, still present) facts for an entity.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the read fails.
    pub fn list_archived(&self, entity_id: i64) -> Result<Vec<Fact>> {
        self.read_facts(
            &format!(
                "SELECT {FACT_COLUMNS} FROM facts WHERE entity_id = ?1 AND archived = 1
                 ORDER BY time, created_at, id"
            ),
            entity_id,
            None,
        )
    }

    /// List every entity id in the store, used when a decay pass scans the
    /// whole tenant instead of a single entity.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the read fails.
    pub fn all_entity_ids(&self) -> Result<Vec<i64>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare("SELECT id FROM entities ORDER BY id")?;
        let mut rows = stmt.query([])?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next()? {
            ids.push(row.get(0)?);
        }
        Ok(ids)
    }

    /// List all entity ids belonging to a single tenant.
    ///
    /// `memory_decay` uses this so a tenant-scoped tool call never touches
    /// entities owned by other tenants (previously the tool scanned every
    /// entity in the database regardless of `tenant_id`).
    pub fn all_entity_ids_in_tenant(&self, tenant_id: &str) -> Result<Vec<i64>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare("SELECT id FROM entities WHERE tenant_id = ?1 ORDER BY id")?;
        let mut rows = stmt.query(params![tenant_id])?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next()? {
            ids.push(row.get(0)?);
        }
        Ok(ids)
    }

    /// Read the current decay state (`weight`, `archived`) for a fact.
    ///
    /// Independent of [`Fact::confidence`]: decaying a fact must not change how
    /// much it is believed.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the read fails.
    pub fn get_decay(&self, fact_id: i64) -> Result<(f64, bool)> {
        let conn = self.lock_conn()?;
        let row = conn.query_row(
            "SELECT weight, archived FROM facts WHERE id = ?1",
            params![fact_id],
            |row| Ok((row.get::<_, f64>(0)?, row.get::<_, i64>(1)? != 0)),
        )?;
        Ok(row)
    }
}

impl FactStore for SqliteFactStore {
    fn insert_fact(&self, fact: &Fact) -> Result<i64> {
        let conn = self.lock_conn()?;
        let mut anchors = std::collections::HashMap::new();
        let mut stmt = conn.prepare(INSERT_FACT_SQL)?;
        let evidence_id = match fact.evidence_id {
            Some(id) => Some(id),
            None => Self::anchor_evidence_on(&conn, &fact.payload, &mut anchors)?,
        };
        Self::bind_fact(&mut stmt, fact, evidence_id)?;
        Ok(conn.last_insert_rowid())
    }

    fn insert_batch(&self, facts: &[Fact]) -> Result<usize> {
        if facts.is_empty() {
            return Ok(0);
        }
        let mut conn = self.lock_conn()?;
        let transaction = conn.transaction()?;
        {
            let mut anchors = std::collections::HashMap::new();
            Self::insert_facts_on(&transaction, facts, &mut anchors)?;
        }
        transaction.commit()?;
        Ok(facts.len())
    }

    fn get_facts(&self, entity_id: i64) -> Result<Vec<Fact>> {
        self.read_facts(
            &format!(
                "SELECT {FACT_COLUMNS} FROM facts WHERE entity_id = ?1 ORDER BY time, created_at, id"
            ),
            entity_id,
            None,
        )
    }

    fn get_facts_by_type(&self, entity_id: i64, fact_type: FactType) -> Result<Vec<Fact>> {
        self.read_facts(
            &format!(
                "SELECT {FACT_COLUMNS} FROM facts WHERE entity_id = ?1 AND fact_type = ?2
                 ORDER BY time, created_at, id"
            ),
            entity_id,
            Some(fact_type),
        )
    }

    fn get_timeline(&self, entity_id: i64) -> Result<Vec<Fact>> {
        self.read_facts(
            &format!(
                "SELECT {FACT_COLUMNS} FROM facts WHERE entity_id = ?1
                 ORDER BY time DESC, created_at DESC, id DESC"
            ),
            entity_id,
            None,
        )
    }

    fn get_fact_by_id(&self, fact_id: i64) -> Result<Option<Fact>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(&format!("SELECT {FACT_COLUMNS} FROM facts WHERE id = ?1"))?;
        let mut rows = stmt.query(params![fact_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::row_to_fact(row)?)),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
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
}
