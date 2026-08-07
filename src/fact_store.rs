//! FactStore — SQLite persistence layer for immutable cognition facts.
//!
//! The implementation uses explicit transactions, strict decoding, and the
//! crate-wide storage error type. No database or serialization failure is
//! converted into a successful-looking zero or empty result.

use std::str::FromStr;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};

use crate::cognition::{Fact, FactStore, FactType};
use crate::error::{Error, Result, StorageError};
use crate::relationship::{EmotionTrend, RelationshipStage, RelationshipState};

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
    weight       REAL DEFAULT 1.0,
    archived     INTEGER DEFAULT 0
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
";

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
        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_entities_tenant_external
                 ON entities(tenant_id, external_key) WHERE external_key IS NOT NULL;
             CREATE INDEX IF NOT EXISTS idx_entities_tenant_name
                 ON entities(tenant_id, name);",
        )?;
        Ok(())
    }

    fn ensure_column(conn: &Connection, table: &str, column: &str, definition: &str) -> Result<()> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            if row.get::<_, String>(1)? == column {
                return Ok(());
            }
        }
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
        Ok(())
    }

    /// Resolve or create a tenant-scoped entity with an optional external key.
    ///
    /// The `(tenant_id, external_key)` pair is the stable identity boundary for
    /// users. Non-user entities can omit `external_key` and remain name based.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the entity cannot be read or created.
    pub fn resolve_entity(
        &self,
        tenant_id: &str,
        external_key: Option<&str>,
        name: &str,
        entity_type: &str,
    ) -> Result<i64> {
        let conn = self.lock_conn()?;
        let existing = if let Some(key) = external_key {
            conn.query_row(
                "SELECT id FROM entities WHERE tenant_id = ?1 AND external_key = ?2",
                params![tenant_id, key],
                |row| row.get(0),
            )
            .optional()?
        } else {
            conn.query_row(
                "SELECT id FROM entities WHERE tenant_id = ?1 AND external_key IS NULL AND name = ?2 AND entity_type = ?3",
                params![tenant_id, name, entity_type],
                |row| row.get(0),
            )
            .optional()?
        };
        if let Some(id) = existing {
            return Ok(id);
        }

        if tenant_id == "default" && external_key == Some("default") && entity_type == "user" {
            let root = conn
                .query_row(
                    "SELECT name, entity_type, external_key FROM entities WHERE id = 1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .optional()?;
            match root {
                None => {
                    conn.execute(
                        "INSERT INTO entities (id, tenant_id, external_key, name, entity_type) VALUES (1, ?1, ?2, ?3, ?4)",
                        params![tenant_id, external_key, name, entity_type],
                    )?;
                    return Ok(1);
                }
                Some((root_name, root_type, None))
                    if root_name.eq_ignore_ascii_case("user") && root_type == "user" =>
                {
                    conn.execute(
                        "UPDATE entities SET tenant_id = ?1, external_key = ?2 WHERE id = 1",
                        params![tenant_id, external_key],
                    )?;
                    return Ok(1);
                }
                Some(_) => {}
            }
        }

        conn.execute(
            "INSERT INTO entities (tenant_id, external_key, name, entity_type) VALUES (?1, ?2, ?3, ?4)",
            params![tenant_id, external_key, name, entity_type],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Resolve or create a tenant-scoped user entity.
    ///
    /// Empty user ids map to `default` for backward compatibility. The returned
    /// id is stable for the same tenant/user pair and isolated from every other
    /// pair.
    ///
    /// # Errors
    ///
    /// Returns a storage error when identity resolution fails.
    pub fn resolve_user(&self, tenant_id: &str, user_id: &str) -> Result<i64> {
        let tenant_id = if tenant_id.trim().is_empty() {
            "default"
        } else {
            tenant_id.trim()
        };
        let user_id = if user_id.trim().is_empty() {
            "default"
        } else {
            user_id.trim()
        };
        let name = if user_id == "default" {
            "User".to_string()
        } else {
            format!("User:{user_id}")
        };
        self.resolve_entity(tenant_id, Some(user_id), &name, "user")
    }

    /// Resolve or create a tenant-scoped **Agent** entity, distinct from every
    /// User entity.
    ///
    /// Agent facts (tool calls, completed actions) are attributed to the Agent
    /// entity so they never pollute the User's cognition channel
    /// (external-knowledge-plan §C2, "agent 不替用户表态"). The `external_key`
    /// is namespaced `agent:<agent_id>` so it cannot collide with user keys
    /// (which are bare `<user_id>`).
    ///
    /// Empty ids map to `default` for backward compatibility. The returned id
    /// is stable for the same tenant/agent pair and isolated from every other
    /// pair.
    ///
    /// # Errors
    ///
    /// Returns a storage error when identity resolution fails.
    pub fn resolve_agent(&self, tenant_id: &str, agent_id: &str) -> Result<i64> {
        let tenant_id = if tenant_id.trim().is_empty() {
            "default"
        } else {
            tenant_id.trim()
        };
        let agent_id = if agent_id.trim().is_empty() {
            "default"
        } else {
            agent_id.trim()
        };
        let name = if agent_id == "default" {
            "Agent".to_string()
        } else {
            format!("Agent:{agent_id}")
        };
        // Namespace the external key so agent identity never collides with a
        // user identity that happens to share the same bare id.
        let external_key = format!("agent:{agent_id}");
        self.resolve_entity(tenant_id, Some(&external_key), &name, "agent")
    }

    /// Resolve an existing tenant-scoped entity.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the lookup fails.
    pub fn find_entity(
        &self,
        tenant_id: &str,
        external_key: Option<&str>,
        name: &str,
    ) -> Result<Option<(i64, String, String)>> {
        let conn = self.lock_conn()?;
        let mut stmt = if external_key.is_some() {
            conn.prepare(
                "SELECT id, name, entity_type FROM entities WHERE tenant_id = ?1 AND external_key = ?2",
            )?
        } else {
            conn.prepare(
                "SELECT id, name, entity_type FROM entities WHERE tenant_id = ?1 AND name = ?2 ORDER BY id LIMIT 1",
            )?
        };
        let key = external_key.unwrap_or(name);
        stmt.query_row(params![tenant_id, key], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()
        .map_err(Error::from)
    }

    fn lock_conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|error| Error::Storage(StorageError::LockPoisoned(error.to_string())))
    }

    fn fact_type_name(fact_type: FactType) -> &'static str {
        match fact_type {
            FactType::Identity => "identity",
            FactType::Preference => "preference",
            FactType::Goal => "goal",
            FactType::Event => "event",
            FactType::Relationship => "relationship",
            FactType::Emotion => "emotion",
            FactType::Location => "location",
            FactType::Occupation => "occupation",
            FactType::Interest => "interest",
            FactType::Habit => "habit",
        }
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
        Ok(Fact {
            id: Some(row.get("id")?),
            entity_id: row.get("entity_id")?,
            fact_type,
            time: row.get("time")?,
            payload,
            evidence_id: row.get("evidence_id")?,
            created_at: row.get("created_at")?,
        })
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
            Some(value) => stmt.query(params![entity_id, Self::fact_type_name(value)])?,
            None => stmt.query(params![entity_id])?,
        };
        let mut facts = Vec::new();
        while let Some(row) = rows.next()? {
            facts.push(Self::row_to_fact(row)?);
        }
        Ok(facts)
    }

    /// Write back a decay score and archive flag for a fact. This is the only
    /// decay write path and it never deletes the row — the fact stays readable
    /// so the persona evolution timeline remains reconstructable.
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
            "SELECT id, entity_id, fact_type, time, payload, evidence_id, created_at
             FROM facts WHERE entity_id = ?1 AND archived = 1 ORDER BY time, created_at, id",
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

    /// Read the current decay flags (`weight`, `archived`) for a fact.
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

    /// Persist (insert or replace) a relationship state row.
    ///
    /// The `(tenant_id, agent_entity_id, user_entity_id)` triple is the unique
    /// identity of a relationship; an existing row is updated in place on
    /// conflict. This is an upsert, not a delete — the latest snapshot always
    /// wins, matching the ADD-only accumulation policy at the state layer.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the row cannot be written.
    pub(crate) fn save_relationship(&self, rs: &RelationshipState) -> Result<i64> {
        let conn = self.lock_conn()?;
        save_relationship_unlocked(&conn, rs)
    }

    /// Load a relationship state row by its identity triple.
    ///
    /// Returns `Ok(None)` when no relationship has been recorded yet.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the row cannot be read or decoded.
    pub(crate) fn load_relationship(
        &self,
        tenant_id: &str,
        agent_entity_id: i64,
        user_entity_id: i64,
    ) -> Result<Option<RelationshipState>> {
        let conn = self.lock_conn()?;
        load_relationship_unlocked(&conn, tenant_id, agent_entity_id, user_entity_id)
    }

    /// Atomically read-modify-write a relationship inside ONE lock critical
    /// section.
    ///
    /// `apply_messages` used to call `load_relationship` and
    /// `save_relationship` as two separate lock acquisitions: two concurrent
    /// calls for the same (agent, user) pair both read the old intimacy, each
    /// added its own delta, and one increment was silently lost. Holding the
    /// connection lock across load → compute → save serializes the pair, so
    /// every delta is preserved.
    ///
    /// `compute` receives the current state (None when the pair has no
    /// relationship yet) and returns the new state to persist.
    pub(crate) fn update_relationship_atomic(
        &self,
        tenant_id: &str,
        agent_entity_id: i64,
        user_entity_id: i64,
        compute: impl FnOnce(Option<&RelationshipState>) -> RelationshipState,
    ) -> Result<RelationshipState> {
        let conn = self.lock_conn()?;
        let current =
            load_relationship_unlocked(&conn, tenant_id, agent_entity_id, user_entity_id)?;
        let updated = compute(current.as_ref());
        save_relationship_unlocked(&conn, &updated)?;
        Ok(updated)
    }
}

/// Upsert a relationship row on an already-locked connection (no lock
/// acquisition — callers hold it).
fn save_relationship_unlocked(conn: &Connection, rs: &RelationshipState) -> Result<i64> {
    conn.execute(
        "INSERT INTO relationship_state
             (tenant_id, agent_entity_id, user_entity_id, intimacy, stage, emotion_trend, recent_topics, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(tenant_id, agent_entity_id, user_entity_id) DO UPDATE SET
             intimacy      = excluded.intimacy,
             stage         = excluded.stage,
             emotion_trend = excluded.emotion_trend,
             recent_topics = excluded.recent_topics,
             updated_at    = excluded.updated_at",
        params![
            rs.tenant_id,
            rs.agent_entity_id,
            rs.user_entity_id,
            rs.intimacy,
            rs.stage.as_str(),
            rs.emotion_trend.as_str(),
            serde_json::to_string(&rs.recent_topics)?,
            rs.updated_at,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Load a relationship row on an already-locked connection.
///
/// Returns `Ok(None)` when no relationship has been recorded yet.
fn load_relationship_unlocked(
    conn: &Connection,
    tenant_id: &str,
    agent_entity_id: i64,
    user_entity_id: i64,
) -> Result<Option<RelationshipState>> {
    let row = conn
        .query_row(
            "SELECT tenant_id, agent_entity_id, user_entity_id, intimacy, stage, emotion_trend, recent_topics, updated_at
             FROM relationship_state
             WHERE tenant_id = ?1 AND agent_entity_id = ?2 AND user_entity_id = ?3",
            params![tenant_id, agent_entity_id, user_entity_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((
        tenant_id,
        agent_entity_id,
        user_entity_id,
        intimacy,
        stage,
        trend,
        topics_json,
        updated_at,
    )) = row
    else {
        return Ok(None);
    };
    let recent_topics: Vec<String> = serde_json::from_str(&topics_json).map_err(|error| {
        Error::Storage(StorageError::InvalidData(format!(
            "relationship recent_topics is not valid JSON: {error}"
        )))
    })?;
    let stage = RelationshipStage::from_str(&stage).map_err(|message| {
        Error::Storage(StorageError::InvalidData(format!(
            "relationship stage `{stage}` is invalid: {message}"
        )))
    })?;
    let emotion_trend = EmotionTrend::from_str(&trend).map_err(|message| {
        Error::Storage(StorageError::InvalidData(format!(
            "relationship emotion_trend `{trend}` is invalid: {message}"
        )))
    })?;
    Ok(Some(RelationshipState {
        tenant_id,
        agent_entity_id,
        user_entity_id,
        intimacy,
        stage,
        emotion_trend,
        recent_topics,
        updated_at,
    }))
}

impl FactStore for SqliteFactStore {
    fn insert_fact(&self, fact: &Fact) -> Result<i64> {
        let payload = serde_json::to_string(&fact.payload)?;
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO facts (entity_id, fact_type, time, payload, evidence_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                fact.entity_id,
                Self::fact_type_name(fact.fact_type),
                fact.time,
                payload,
                fact.evidence_id,
                fact.created_at,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    fn insert_batch(&self, facts: &[Fact]) -> Result<usize> {
        if facts.is_empty() {
            return Ok(0);
        }
        let mut conn = self.lock_conn()?;
        let transaction = conn.transaction()?;
        {
            let mut stmt = transaction.prepare(
                "INSERT INTO facts (entity_id, fact_type, time, payload, evidence_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for fact in facts {
                let payload = serde_json::to_string(&fact.payload)?;
                stmt.execute(params![
                    fact.entity_id,
                    Self::fact_type_name(fact.fact_type),
                    fact.time,
                    payload,
                    fact.evidence_id,
                    fact.created_at,
                ])?;
            }
        }
        transaction.commit()?;
        Ok(facts.len())
    }

    fn get_facts(&self, entity_id: i64) -> Result<Vec<Fact>> {
        self.read_facts(
            "SELECT id, entity_id, fact_type, time, payload, evidence_id, created_at
             FROM facts WHERE entity_id = ?1 ORDER BY time, created_at, id",
            entity_id,
            None,
        )
    }

    fn get_facts_by_type(&self, entity_id: i64, fact_type: FactType) -> Result<Vec<Fact>> {
        self.read_facts(
            "SELECT id, entity_id, fact_type, time, payload, evidence_id, created_at
             FROM facts WHERE entity_id = ?1 AND fact_type = ?2 ORDER BY time, created_at, id",
            entity_id,
            Some(fact_type),
        )
    }

    fn get_timeline(&self, entity_id: i64) -> Result<Vec<Fact>> {
        self.read_facts(
            "SELECT id, entity_id, fact_type, time, payload, evidence_id, created_at
             FROM facts WHERE entity_id = ?1 ORDER BY time DESC, created_at DESC, id DESC",
            entity_id,
            None,
        )
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

    /// Objective: Verify user identities are stable and tenant isolated.
    /// Invariants: Same tenant/user resolves once; changing either component changes the entity id.
    #[test]
    fn user_identity_is_stable_and_tenant_isolated() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        let default_id = store
            .resolve_user("default", "")
            .expect("resolve backward-compatible root user");
        let default_again = store
            .resolve_user("default", "default")
            .expect("resolve the same root user again");
        let tenant_a_user = store
            .resolve_user("tenant-a", "alice")
            .expect("resolve tenant A user");
        let tenant_b_user = store
            .resolve_user("tenant-b", "alice")
            .expect("resolve tenant B user");
        let tenant_a_other = store
            .resolve_user("tenant-a", "bob")
            .expect("resolve second tenant A user");

        assert_eq!(
            default_id, 1,
            "The compatible default User root must retain id 1"
        );
        assert_eq!(
            default_again, default_id,
            "Repeated identity resolution must be stable"
        );
        assert_ne!(
            tenant_a_user, tenant_b_user,
            "The same user id in different tenants must not collide"
        );
        assert_ne!(
            tenant_a_user, tenant_a_other,
            "Different users in one tenant must not collide"
        );
    }

    /// Objective: Verify agent identities are stable, tenant-isolated, and NEVER
    /// collide with a user identity that shares the same bare id — the core
    /// zero-pollution boundary for the agent fact channel.
    /// Invariants: resolve_agent is stable across calls; agent id != user id
    /// for the same bare id; tenant isolation holds; default agent resolves.
    #[test]
    fn agent_identity_is_distinct_from_user_identity() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        let agent_default = store
            .resolve_agent("default", "")
            .expect("resolve default agent");
        let agent_default_again = store
            .resolve_agent("default", "default")
            .expect("resolve default agent again");
        assert_eq!(
            agent_default, agent_default_again,
            "repeated agent resolution must be stable"
        );

        // The same bare id "alice" MUST resolve to different entities when used
        // as a user vs an agent — agents never pollute the user channel.
        let user_alice = store
            .resolve_user("tenant-a", "alice")
            .expect("resolve user alice");
        let agent_alice = store
            .resolve_agent("tenant-a", "alice")
            .expect("resolve agent alice");
        assert_ne!(
            user_alice, agent_alice,
            "agent entity must never collide with a user entity sharing the same bare id"
        );

        // Tenant isolation: the same agent id in two tenants resolves differently.
        let agent_a = store
            .resolve_agent("tenant-a", "bot")
            .expect("resolve tenant-a bot");
        let agent_b = store
            .resolve_agent("tenant-b", "bot")
            .expect("resolve tenant-b bot");
        assert_ne!(
            agent_a, agent_b,
            "the same agent id in different tenants must not collide"
        );

        // The agent entity is typed "agent", not "user".
        let conn = store.lock_conn().expect("lock");
        let entity_type: String = conn
            .query_row(
                "SELECT entity_type FROM entities WHERE id = ?1",
                params![agent_alice],
                |row| row.get(0),
            )
            .expect("read agent entity type");
        assert_eq!(
            entity_type, "agent",
            "agent entity must be typed 'agent' so it is distinguishable from users"
        );
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
}
