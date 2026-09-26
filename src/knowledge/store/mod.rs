//! SQLite store for the general knowledge model.
//!
//! Mirrors [`crate::character::SQLiteCharacterStore`] in shape (a single
//! `Arc<Mutex<Connection>>`, idempotent `init`, `Box<dyn ToSql>` for optional
//! filters) but targets the eight general-model tables whose DDL lives in
//! [`crate::storage::schema::KNOWLEDGE_SCHEMA`].
//!
//! Beyond raw CRUD, it implements the four high-level queries that back the
//! MCP tools in dev_guide §5: `inspect_entity`, `entity_timeline`,
//! `relation_graph` (BFS via petgraph), and `search_evidence`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use tokio::sync::Mutex;

use crate::error::{Error, Result, StorageError};
use crate::storage::{KNOWLEDGE_SCHEMA, WORLD_SCHEMA};

use super::{
    Chapter, CompilerRun, Document, EntityProfileEntry, Evidence, EvidenceHit, EvidenceSourceType,
    GraphEdge, GraphNode, InspectEntityResult, KnowledgeEdge, KnowledgeEvidenceLink,
    KnowledgeObject, Mention, ObjectType, Origin, RelationGraphResult, TimelineEntry,
};

/// Convert a rusqlite row into a [`Document`].
mod queries;
mod trait_def;
mod trait_impl;
mod types;
mod world_io;

pub use trait_def::KnowledgeStore;
pub use types::{
    EventParticipantRef, GraphCounts, NewWorldEvent, NewWorldState, WorldEntity, WorldEvent,
    WorldProfile, WorldRelation, WorldState,
};

use types::{
    json_to_string, row_to_chapter, row_to_document, row_to_edge, row_to_evidence, row_to_mention,
    row_to_object,
};

/// Add `column` to `table` when it is missing (idempotent).
///
/// `CREATE TABLE IF NOT EXISTS` never alters an existing table, so columns
/// introduced after a database was first created need an explicit ALTER.
/// Returns `Ok(())` whether or not the column already existed.
fn ensure_column(conn: &Connection, table: &str, column: &str, definition: &str) -> Result<()> {
    let mut exists = false;
    {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            if row.get::<_, String>(1)? == column {
                exists = true;
                break;
            }
        }
    }
    if !exists {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))
        .map_err(|e| StorageError::Schema(format!("add {table}.{column}: {e}")))?;
    }
    Ok(())
}

/// Create a unique index idempotently, deduplicating first when needed.
///
/// `CREATE UNIQUE INDEX IF NOT EXISTS` is skipped when the index already
/// exists; when the index is missing but violating rows exist (the
/// cross-connection race that this index exists to prevent), `dedupe_sql`
/// collapses each identity group to its freshest row so the create succeeds.
fn ensure_unique_index(
    conn: &Connection,
    name: &str,
    create_sql: &str,
    dedupe_sql: &str,
) -> Result<()> {
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
            params![name],
            |r| r.get(0),
        )
        .map_err(|e| StorageError::Schema(format!("check index {name}: {e}")))?;
    if exists > 0 {
        return Ok(());
    }
    if let Err(e) = conn.execute_batch(dedupe_sql) {
        // Empty table (the common case) makes the DELETE a no-op; a failure
        // here must not brick init — the create below is the gate.
        tracing::warn!(error = %e, index = name, "pre-index dedupe skipped");
    }
    conn.execute_batch(create_sql)
        .map_err(|e| StorageError::Schema(format!("create index {name}: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests;

/// SQLite implementation of [`KnowledgeStore`].
pub struct SQLiteKnowledgeStore {
    conn: Arc<Mutex<Connection>>,
}

impl SQLiteKnowledgeStore {
    /// Open a file-backed store and initialize the schema idempotently.
    ///
    /// # Errors
    /// Returns [`StorageError::Schema`] if the file cannot be opened or the
    /// schema DDL fails.
    pub async fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)
            .map_err(|e| StorageError::Schema(format!("open knowledge store: {e}")))?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.init().await?;
        Ok(store)
    }

    /// Open an in-memory store (for tests).
    pub async fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| StorageError::Schema(format!("open in-memory: {e}")))?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.init().await?;
        Ok(store)
    }

    async fn init(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        // busy_timeout makes concurrent connections wait (up to 5s) for a lock
        // instead of failing immediately. foreign_keys enforces declared FK
        // constraints (otherwise they're cosmetic and orphan rows can be
        // inserted). WAL is intentionally NOT enabled: its `-wal`/`-shm` sidecar
        // files don't always survive cleanly across separate processes (e.g.
        // nextest test processes), causing "file is not a database" on the next
        // open. The default rollback journal is process-safe for our access
        // pattern (serialized writers, concurrent readers).
        conn.execute_batch(
            "PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;",
        )?;
        conn.execute_batch(KNOWLEDGE_SCHEMA)
            .map_err(|e| StorageError::Schema(format!("init knowledge schema: {e}")))?;
        // WORLD_SCHEMA (V7 entity-centric tables: entities/aliases/profiles/
        // events/…) was declared in `storage::schema` but never executed —
        // the doc comment claimed `init` ran it, yet only KNOWLEDGE_SCHEMA
        // did (CODE_REVIEW C10). Executing it here is idempotent
        // (CREATE TABLE IF NOT EXISTS) and brings the V7 general model live
        // as the destination for DocumentSource/domain-pack output.
        conn.execute_batch(WORLD_SCHEMA)
            .map_err(|e| StorageError::Schema(format!("init world schema: {e}")))?;
        // `CREATE TABLE IF NOT EXISTS` never adds columns to an existing
        // table — databases created before the events offset columns need an
        // idempotent ALTER (same pattern as fact_store::ensure_column).
        ensure_column(&conn, "events", "start_offset", "INTEGER")?;
        ensure_column(&conn, "events", "end_offset", "INTEGER")?;
        // Document provenance column (T12): databases created before
        // (title, source) identity need the column added — existing rows
        // backfill to '' via the DEFAULT, which the write path treats as
        // "untagged legacy document".
        ensure_column(&conn, "documents", "source", "TEXT NOT NULL DEFAULT ''")?;
        // Unique expression indexes backing the atomic ON CONFLICT upserts in
        // `world_io.rs`. They MUST be created here (not in WORLD_SCHEMA) and
        // AFTER ensure_column: on a legacy DB the offset columns only exist
        // after the ALTER above, and SQLite rejects an index referencing
        // missing columns. A failed index would in turn make every
        // `ON CONFLICT(...)` upsert fail at statement level.
        ensure_unique_index(
            &conn,
            "ux_events_identity",
            "CREATE UNIQUE INDEX IF NOT EXISTS ux_events_identity ON events(\
             title, IFNULL(timestamp, -1), IFNULL(start_offset, -1), IFNULL(end_offset, -1))",
            // True duplicates can only come from the pre-index cross-connection
            // race this index eliminates; keep the freshest row (MAX id), which
            // matches the DO UPDATE refresh semantics.
            "DELETE FROM events WHERE id NOT IN (\
             SELECT MAX(id) FROM events \
             GROUP BY title, IFNULL(timestamp, -1), IFNULL(start_offset, -1), \
                      IFNULL(end_offset, -1))",
        )?;
        ensure_unique_index(
            &conn,
            "ux_world_states_identity",
            "CREATE UNIQUE INDEX IF NOT EXISTS ux_world_states_identity ON world_states(\
             entity_id, slot, IFNULL(event_id, -1), IFNULL(chapter, -1))",
            "DELETE FROM world_states WHERE id NOT IN (\
             SELECT MAX(id) FROM world_states \
             GROUP BY entity_id, slot, IFNULL(event_id, -1), IFNULL(chapter, -1))",
        )?;
        Ok(())
    }

    /// Toggle FK enforcement on this connection. The migrator disables FKs for
    /// the duration of a run (it inserts in the correct parent→child order, so
    /// enforcement is unnecessary, and cross-novel edges can transiently
    /// reference not-yet-migrated objects). Production queries keep FKs ON.
    pub async fn set_foreign_keys_enabled(&self, on: bool) -> Result<()> {
        let conn = self.conn.lock().await;
        let sql = if on {
            "PRAGMA foreign_keys = ON"
        } else {
            "PRAGMA foreign_keys = OFF"
        };
        conn.execute(sql, [])?;
        Ok(())
    }

    /// Begin an explicit SQLite transaction on the shared connection.
    ///
    /// All subsequent store calls on this connection participate in the
    /// transaction until [`commit_transaction`](Self::commit_transaction) or
    /// [`rollback_transaction`](Self::rollback_transaction) ends it. Used by
    /// the migrator to make a full run atomic (H6): a failure mid-way rolls
    /// back every row written so far instead of leaving a half-migrated
    /// database.
    ///
    /// # Errors
    ///
    /// Returns a storage error if `BEGIN` fails (e.g. a transaction is already
    /// open on this connection — callers must not nest).
    pub async fn begin_transaction(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch("BEGIN;").map_err(Into::into)
    }

    /// Commit the transaction opened by [`begin_transaction`](Self::begin_transaction).
    ///
    /// # Errors
    ///
    /// Returns a storage error if `COMMIT` fails.
    pub async fn commit_transaction(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch("COMMIT;").map_err(Into::into)
    }

    /// Roll back the transaction opened by [`begin_transaction`](Self::begin_transaction),
    /// discarding every write made since it began.
    ///
    /// # Errors
    ///
    /// Returns a storage error if `ROLLBACK` fails.
    pub async fn rollback_transaction(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch("ROLLBACK;").map_err(Into::into)
    }

    /// Report whether a transaction is currently open on this connection.
    ///
    /// Used by callers like `Migrator::migrate` that would otherwise nest a
    /// `BEGIN` inside a caller-owned transaction (SQLite rejects nested
    /// BEGIN, and a failed `begin_transaction` would corrupt the outer
    /// transaction's state). When this returns `true`, the caller must run
    /// its inner work directly without opening or closing its own transaction.
    pub async fn in_transaction(&self) -> Result<bool> {
        let conn = self.conn.lock().await;
        Ok(!conn.is_autocommit())
    }

    /// Drop all general-model rows. Called by the migrator at the start of a
    /// full run so a re-migration is a clean rebuild rather than an
    /// accumulating append.
    ///
    /// FK enforcement is temporarily disabled during the wipe: the tables may
    /// contain rows inserted by other connections (e.g. test helpers using raw
    /// `rusqlite::Connection`s without `foreign_keys = ON`) that violate FK
    /// constraints, and we delete everything anyway, so enforcing FKs here only
    /// risks a spurious "FOREIGN KEY constraint failed" on the parent deletes.
    pub async fn clear_all(&self) -> Result<()> {
        // If a caller already opened a transaction (e.g. `Migrator::migrate`'s
        // H6 wrap), we must NOT nest a `BEGIN` — SQLite rejects it with
        // "cannot start a transaction within a transaction". Run the DELETE
        // directly inside the caller's transaction instead; the caller owns
        // the atomicity. Otherwise wrap for atomicity (PRAGMA outside, DELETE
        // inside, rollback on failure) so FK never stays OFF with a half-wipe.
        let in_transaction = {
            let conn = self.conn.lock().await;
            !conn.is_autocommit()
        };
        if in_transaction {
            return self.clear_all_inner().await;
        }
        self.set_foreign_keys_enabled(false).await?;
        self.begin_transaction().await?;
        let result = self.clear_all_inner().await;
        match &result {
            Ok(_) => {
                // COMMIT first: `PRAGMA foreign_keys` is a no-op inside an
                // open transaction, so re-enabling BEFORE commit left FK
                // enforcement OFF for the connection's remaining lifetime.
                self.commit_transaction().await?;
                let _ = self.set_foreign_keys_enabled(true).await;
            }
            Err(_) => {
                let _ = self.rollback_transaction().await;
                let _ = self.set_foreign_keys_enabled(true).await;
            }
        }
        result
    }

    async fn clear_all_inner(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch(
            "DELETE FROM knowledge_evidence;
             DELETE FROM mentions;
             DELETE FROM knowledge_edges;
             DELETE FROM evidence;
             DELETE FROM knowledge_objects;
             DELETE FROM compiler_runs;
             DELETE FROM chapters;
             DELETE FROM documents;",
        )?;
        Ok(())
    }

    /// Drop all rows belonging to a single document (chapters, objects, edges,
    /// evidence, mentions, compiler_runs). Used by the migrator to make
    /// re-migrating one novel a clean rebuild without touching other novels'
    /// data. FK enforcement is disabled during the wipe for the same reason as
    /// [`clear_all`].
    pub async fn clear_for_document(&self, doc_id: i64) -> Result<()> {
        // Same outer-transaction detection as clear_all: never nest a BEGIN
        // inside a caller transaction (Migrator::migrate's H6 wrap calls us);
        // the caller owns atomicity then.
        let in_transaction = {
            let conn = self.conn.lock().await;
            !conn.is_autocommit()
        };
        if in_transaction {
            return self.clear_for_document_inner(doc_id).await;
        }
        self.set_foreign_keys_enabled(false).await?;
        self.begin_transaction().await?;
        let result = self.clear_for_document_inner(doc_id).await;
        match &result {
            Ok(_) => {
                // COMMIT first: PRAGMA foreign_keys is a no-op in a transaction.
                self.commit_transaction().await?;
                let _ = self.set_foreign_keys_enabled(true).await;
            }
            Err(_) => {
                let _ = self.rollback_transaction().await;
                let _ = self.set_foreign_keys_enabled(true).await;
            }
        }
        result
    }

    async fn clear_for_document_inner(&self, doc_id: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch(&format!(
            "DELETE FROM knowledge_evidence
               WHERE source_type = 'object' AND source_id IN
                 (SELECT id FROM knowledge_objects WHERE doc_id = {doc_id})
               OR source_type = 'edge' AND source_id IN
                 (SELECT id FROM knowledge_edges WHERE source_id IN
                   (SELECT id FROM knowledge_objects WHERE doc_id = {doc_id})
                 OR target_id IN
                   (SELECT id FROM knowledge_objects WHERE doc_id = {doc_id}));
             DELETE FROM mentions WHERE object_id IN
               (SELECT id FROM knowledge_objects WHERE doc_id = {doc_id});
             DELETE FROM knowledge_edges WHERE source_id IN
               (SELECT id FROM knowledge_objects WHERE doc_id = {doc_id})
               OR target_id IN
               (SELECT id FROM knowledge_objects WHERE doc_id = {doc_id});
             DELETE FROM evidence WHERE doc_id = {doc_id};
             DELETE FROM knowledge_objects WHERE doc_id = {doc_id};
             DELETE FROM compiler_runs WHERE doc_id = {doc_id};
             DELETE FROM chapters WHERE doc_id = {doc_id};",
        ))?;
        Ok(())
    }
}
