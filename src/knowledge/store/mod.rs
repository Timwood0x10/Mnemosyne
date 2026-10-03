//! SQLite store for the general knowledge model.
//!
//! Mirrors [`crate::character::SQLiteCharacterStore`] in shape (a single
//! `Arc<Mutex<Connection>>`, idempotent `init`, `Box<dyn ToSql>` for optional
//! filters) but targets the eight general-model tables whose DDL lives in
//! [`crate::persistence::schema::KNOWLEDGE_SCHEMA`].
//!
//! Beyond raw CRUD, it implements the four high-level queries that back the
//! MCP tools in dev_guide §5: `inspect_entity`, `entity_timeline`,
//! `relation_graph` (BFS via petgraph), and `search_evidence`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::error::{Error, Result, StorageError};
use crate::persistence::unique_index::{UniqueIndex, ensure_unique_index};
use crate::persistence::{KNOWLEDGE_SCHEMA, WORLD_SCHEMA};

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

/// The unique identities the world model relies on.
const UNIQUE_INDEXES: &[UniqueIndex] = &[
    UniqueIndex {
        name: "ux_events_identity",
        table: "events",
        create_sql: "CREATE UNIQUE INDEX IF NOT EXISTS ux_events_identity ON events(\
                     title, IFNULL(timestamp, -1), IFNULL(start_offset, -1), \
                     IFNULL(end_offset, -1))",
        identity: &[
            "title",
            "IFNULL(timestamp, -1)",
            "IFNULL(start_offset, -1)",
            "IFNULL(end_offset, -1)",
        ],
        referencing: &[
            ("event_participants", "event_id"),
            ("world_states", "event_id"),
        ],
    },
    UniqueIndex {
        name: "ux_world_states_identity",
        table: "world_states",
        create_sql: "CREATE UNIQUE INDEX IF NOT EXISTS ux_world_states_identity ON world_states(\
                     entity_id, slot, IFNULL(event_id, -1), IFNULL(chapter, -1))",
        identity: &[
            "entity_id",
            "slot",
            "IFNULL(event_id, -1)",
            "IFNULL(chapter, -1)",
        ],
        referencing: &[],
    },
    UniqueIndex {
        name: "ux_documents_identity",
        table: "documents",
        create_sql: "CREATE UNIQUE INDEX IF NOT EXISTS ux_documents_identity \
                     ON documents(title, source)",
        identity: &["title", "source"],
        // Every write path is "find_document → None → create_document" under
        // two separate locks, so two compilers of the same work both saw `None`
        // and both inserted: one document became two rows and every
        // object/edge/evidence row split between them (`clear_for_document`
        // then wiped only half, so a re-migration duplicated the graph). The
        // other tables the compiler writes were already unique-upserted; this
        // is the one that was not.
        referencing: &[
            ("chapters", "doc_id"),
            ("knowledge_objects", "doc_id"),
            ("evidence", "doc_id"),
            ("compiler_runs", "doc_id"),
        ],
    },
];

#[cfg(test)]
mod tests;

/// SQLite implementation of [`KnowledgeStore`].
pub struct SQLiteKnowledgeStore {
    conn: Arc<Mutex<Connection>>,
}

impl SQLiteKnowledgeStore {
    /// Run `work` against the connection on the blocking pool.
    ///
    /// `rusqlite` is synchronous, so every SQL statement the store issues has
    /// to leave the tokio worker instead of running on it: otherwise a slow
    /// query stalls every unrelated connection the runtime is driving (audit
    /// 09-26/H7 — the MCP handlers had the same defect one layer up, fixed in
    /// batch F; the store methods were the remaining source, batch G). The
    /// `Arc` is cloned first so the closure owns its handle, and the connection
    /// guard lives entirely *inside* the closure, so no lock is ever held
    /// across an `await`.
    ///
    /// A poisoned mutex is deliberately recovered rather than propagated: the
    /// connection is a serialized resource whose statements are individually
    /// atomic, and the `tokio::sync::Mutex` this replaced has no poisoning at
    /// all — surfacing poison would let one panicking caller brick the store
    /// for every later one.
    async fn with_conn<F, T>(&self, work: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        crate::blocking::run(move || {
            let guard = conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            work(&guard)
        })
        .await
    }

    /// Open a file-backed store and initialize the schema idempotently.
    ///
    /// # Errors
    /// Returns [`StorageError::Schema`] if the file cannot be opened or the
    /// schema DDL fails.
    pub async fn open(path: &str) -> Result<Self> {
        // `Connection::open` is synchronous too: on a network or slow disk it
        // blocks the tokio worker that awaits it, exactly like the queries
        // `with_conn` already moved off the worker (audit 09-26/H7).
        let path = path.to_owned();
        let conn = crate::blocking::run(move || {
            let conn = Connection::open(&path)
                .map_err(|e| StorageError::Schema(format!("open knowledge store: {e}")))?;
            Ok(conn)
        })
        .await?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.init().await?;
        Ok(store)
    }

    /// Open an in-memory store (for tests).
    pub async fn open_in_memory() -> Result<Self> {
        let conn = crate::blocking::run(|| {
            let conn = Connection::open_in_memory()
                .map_err(|e| StorageError::Schema(format!("open in-memory: {e}")))?;
            Ok(conn)
        })
        .await?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.init().await?;
        Ok(store)
    }

    async fn init(&self) -> Result<()> {
        self.with_conn(|conn| {
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
            // events/…) was declared in `persistence::schema` but never executed —
            // the doc comment claimed `init` ran it, yet only KNOWLEDGE_SCHEMA
            // did (CODE_REVIEW C10). Executing it here is idempotent
            // (CREATE TABLE IF NOT EXISTS) and brings the V7 general model live
            // as the destination for DocumentSource/domain-pack output.
            conn.execute_batch(WORLD_SCHEMA)
                .map_err(|e| StorageError::Schema(format!("init world schema: {e}")))?;
            // `CREATE TABLE IF NOT EXISTS` never adds columns to an existing
            // table — databases created before the events offset columns need an
            // idempotent ALTER (same pattern as fact_store::ensure_column).
            ensure_column(conn, "events", "start_offset", "INTEGER")?;
            ensure_column(conn, "events", "end_offset", "INTEGER")?;
            // Document provenance column (T12): databases created before
            // (title, source) identity need the column added — existing rows
            // backfill to '' via the DEFAULT, which the write path treats as
            // "untagged legacy document".
            ensure_column(conn, "documents", "source", "TEXT NOT NULL DEFAULT ''")?;
            // Unique expression indexes backing the atomic ON CONFLICT upserts in
            // `world_io.rs`. They MUST be created here (not in WORLD_SCHEMA) and
            // AFTER ensure_column: on a legacy DB the offset columns only exist
            // after the ALTER above, and SQLite rejects an index referencing
            // missing columns. A failed index would in turn make every
            // `ON CONFLICT(...)` upsert fail at statement level — which is why a
            // database that predates the index is REPAIRED here (duplicates
            // collapsed, dependents re-pointed) rather than refused: the previous
            // revision's best-effort dedupe left exactly those databases
            // unopenable.
            for spec in UNIQUE_INDEXES {
                ensure_unique_index(conn, spec)?;
            }
            Ok(())
        })
        .await
    }

    /// Toggle FK enforcement on this connection. The migrator disables FKs for
    /// the duration of a run (it inserts in the correct parent→child order, so
    /// enforcement is unnecessary, and cross-novel edges can transiently
    /// reference not-yet-migrated objects). Production queries keep FKs ON.
    pub async fn set_foreign_keys_enabled(&self, on: bool) -> Result<()> {
        self.with_conn(move |conn| {
            let sql = if on {
                "PRAGMA foreign_keys = ON"
            } else {
                "PRAGMA foreign_keys = OFF"
            };
            conn.execute(sql, [])?;
            Ok(())
        })
        .await
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
        // wrap), we must NOT nest a `BEGIN` — SQLite rejects it with
        // "cannot start a transaction within a transaction". Run the DELETE
        // directly inside the caller's transaction instead; the caller owns
        // the atomicity. Otherwise wrap it: FK off, DELETE inside, and
        // enforcement restored on every exit path (see
        // [`Self::with_foreign_keys_disabled`]).
        if self.in_transaction().await? {
            return self.clear_all_inner().await;
        }
        self.with_foreign_keys_disabled(|| self.clear_all_inner())
            .await
    }

    /// Run `work` inside a transaction with FK enforcement temporarily off,
    /// restoring enforcement on EVERY exit path.
    ///
    /// The shape this replaces — disable, `BEGIN`, work, `COMMIT`, enable — is
    /// unsafe twice over, because `PRAGMA foreign_keys` is connection-scoped
    /// and a documented no-op inside a transaction:
    ///
    /// - A `?` on `BEGIN` (the `is_autocommit` check and the `BEGIN` are not
    ///   atomic, so a concurrent `BEGIN` can win) or on `COMMIT`
    ///   (`SQLITE_BUSY`, a full disk) returned **before** enforcement was
    ///   restored, and the connection then kept writing without referential
    ///   integrity for the rest of its life — exactly the state this dance
    ///   exists to prevent.
    /// - Re-enabling while a transaction is still open is silently ignored, so
    ///   even the paths that did remember to restore were no-ops whenever the
    ///   commit or rollback itself had failed.
    ///
    /// Callers therefore cannot forget, and the restore only ever runs from an
    /// autocommit connection.
    ///
    /// # Errors
    ///
    /// Returns the work's error when the work failed, or the restoration error
    /// when the work succeeded but enforcement could not be re-established —
    /// the second is strictly worse, since the connection is unsafe until it is
    /// reopened.
    pub async fn with_foreign_keys_disabled<F, Fut, T>(&self, work: F) -> Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        self.set_foreign_keys_enabled(false).await?;
        // `BEGIN` decides whether the transaction is OURS. If it fails — this
        // connection is already inside a caller's transaction, which `migrate`
        // and `clear_all` do on purpose — the caller owns that transaction and
        // we must not touch it: rolling back somebody else's transaction would
        // silently discard their work.
        let (outcome, transaction_is_ours) = match self.begin_transaction().await {
            Ok(()) => {
                let outcome = match work().await {
                    Ok(value) => self.commit_transaction().await.map(|()| value),
                    Err(work_error) => {
                        if let Err(rollback_error) = self.rollback_transaction().await {
                            // Left open by a failed rollback;
                            // `restore_foreign_keys` finishes it below.
                            tracing::error!(
                                error = %rollback_error,
                                "rolling back the wrapped work failed"
                            );
                        }
                        Err(work_error)
                    }
                };
                (outcome, true)
            }
            Err(begin_error) => (Err(begin_error), false),
        };
        let restored = self.restore_foreign_keys(transaction_is_ours).await;
        match (outcome, restored) {
            (Ok(value), Ok(())) => Ok(value),
            (Ok(_), Err(restore_error)) => Err(restore_error),
            (Err(work_error), Ok(())) => Err(work_error),
            (Err(work_error), Err(restore_error)) => {
                // Both failed: report what the caller asked about, but the
                // connection is now writing without FK enforcement, which an
                // operator has to hear about.
                tracing::error!(
                    error = %restore_error,
                    "restoring foreign keys failed after a failed transaction; \
                     the connection is running without referential integrity"
                );
                Err(work_error)
            }
        }
    }

    /// Re-enable FK enforcement, from an autocommit connection.
    ///
    /// The pragma is ignored inside a transaction, so a `COMMIT`/`ROLLBACK`
    /// that failed would leave the transaction open and the change silently
    /// dropped — the connection would keep going with enforcement off. When
    /// that transaction is one this helper opened (`transaction_is_ours`), one
    /// more `ROLLBACK` returns the connection to autocommit first; a
    /// transaction opened by the caller is left strictly alone.
    async fn restore_foreign_keys(&self, transaction_is_ours: bool) -> Result<()> {
        self.with_conn(move |conn| {
            if transaction_is_ours && !conn.is_autocommit() {
                tracing::warn!(
                    "a transaction outlived its commit/rollback; rolling it back \
                     before restoring foreign keys"
                );
                conn.execute_batch("ROLLBACK")?;
            }
            Ok(())
        })
        .await?;
        self.set_foreign_keys_enabled(true).await
    }

    async fn clear_all_inner(&self) -> Result<()> {
        self.with_conn(|conn| {
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
        })
        .await
    }

    /// Drop all rows belonging to a single document (chapters, objects, edges,
    /// evidence, mentions, compiler_runs). Used by the migrator to make
    /// re-migrating one novel a clean rebuild without touching other novels'
    /// data. FK enforcement is disabled during the wipe for the same reason as
    /// [`clear_all`].
    pub async fn clear_for_document(&self, doc_id: i64) -> Result<()> {
        // Same outer-transaction detection as clear_all: never nest a BEGIN
        // inside a caller transaction (Migrator::migrate's wrap calls us); the
        // caller owns atomicity then.
        if self.in_transaction().await? {
            return self.clear_for_document_inner(doc_id).await;
        }
        self.with_foreign_keys_disabled(|| self.clear_for_document_inner(doc_id))
            .await
    }

    async fn clear_for_document_inner(&self, doc_id: i64) -> Result<()> {
        self.with_conn(move |conn| {
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
        })
        .await
    }
}
