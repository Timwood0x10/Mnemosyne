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
mod types;
mod world_io;

pub use trait_def::KnowledgeStore;
pub use types::{GraphCounts, WorldEntity, WorldEvent, WorldProfile, WorldRelation, WorldState};

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

#[async_trait]
impl KnowledgeStore for SQLiteKnowledgeStore {
    async fn create_document(&self, d: &Document) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO documents (title, author, doc_type, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![d.title, d.author, d.doc_type, d.created_at],
        )?;
        Ok(conn.last_insert_rowid())
    }

    async fn find_document_by_title(&self, title: &str) -> Result<Option<Document>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT * FROM documents WHERE title = ?1")?;
        let mut rows = stmt.query_map(params![title], row_to_document)?;
        match rows.next() {
            Some(Ok(d)) => Ok(Some(d)),
            Some(Err(e)) => Err(StorageError::Sqlite(format!("find_document: {e}")).into()),
            None => Ok(None),
        }
    }

    async fn create_chapter(&self, c: &Chapter) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO chapters (doc_id, chapter_no, title, content, start_offset, end_offset)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                c.doc_id,
                c.chapter_no,
                c.title,
                c.content,
                c.start_offset,
                c.end_offset
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    async fn get_chapter_by_no(&self, doc_id: i64, chapter_no: i32) -> Result<Option<Chapter>> {
        let conn = self.conn.lock().await;
        let mut stmt =
            conn.prepare("SELECT * FROM chapters WHERE doc_id = ?1 AND chapter_no = ?2")?;
        let mut rows = stmt.query_map(params![doc_id, chapter_no], row_to_chapter)?;
        match rows.next() {
            Some(Ok(c)) => Ok(Some(c)),
            Some(Err(e)) => Err(StorageError::Sqlite(format!("get_chapter: {e}")).into()),
            None => Ok(None),
        }
    }

    async fn create_object(&self, o: &KnowledgeObject) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO knowledge_objects (doc_id, object_type, name, properties, confidence, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                o.doc_id,
                o.object_type.as_str(),
                o.name,
                json_to_string(&o.properties),
                o.confidence,
                o.created_at,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    async fn update_object_properties(
        &self,
        id: i64,
        properties: &serde_json::Value,
        confidence: Option<f64>,
    ) -> Result<usize> {
        self.update_object_properties_query(id, properties, confidence)
            .await
    }

    async fn get_object(&self, id: i64) -> Result<Option<KnowledgeObject>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT * FROM knowledge_objects WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![id], row_to_object)?;
        match rows.next() {
            Some(Ok(o)) => Ok(Some(o)),
            Some(Err(e)) => Err(StorageError::Sqlite(format!("get_object: {e}")).into()),
            None => Ok(None),
        }
    }

    async fn get_objects_bulk(&self, ids: &[i64]) -> Result<Vec<KnowledgeObject>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // One IN-clause query replaces N per-id lookups (inspect_entity /
        // entity_timeline previously did one `get_object` per event/neighbor).
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!("SELECT * FROM knowledge_objects WHERE id IN ({placeholders})");
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), row_to_object)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    async fn find_object_by_name(
        &self,
        name: &str,
        doc_id: Option<i64>,
    ) -> Result<Option<KnowledgeObject>> {
        let conn = self.conn.lock().await;
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match doc_id {
            Some(d) => (
                "SELECT * FROM knowledge_objects WHERE name = ?1 AND doc_id = ?2 \
                 ORDER BY confidence DESC LIMIT 1"
                    .to_string(),
                vec![
                    Box::new(name.to_string()) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(d),
                ],
            ),
            None => (
                "SELECT * FROM knowledge_objects WHERE name = ?1 \
                 ORDER BY confidence DESC LIMIT 1"
                    .to_string(),
                vec![Box::new(name.to_string()) as Box<dyn rusqlite::types::ToSql>],
            ),
        };
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query_map(
            rusqlite::params_from_iter(params_vec.iter().map(|p| p.as_ref())),
            row_to_object,
        )?;
        match rows.next() {
            Some(Ok(o)) => Ok(Some(o)),
            Some(Err(e)) => Err(StorageError::Sqlite(format!("find_object_by_name: {e}")).into()),
            None => Ok(None),
        }
    }

    async fn find_object_by_alias(
        &self,
        name: &str,
        doc_id: Option<i64>,
    ) -> Result<Option<KnowledgeObject>> {
        self.find_object_by_alias_query(name, doc_id).await
    }

    async fn create_edge(&self, e: &KnowledgeEdge) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO knowledge_edges
                (source_id, target_id, predicate, properties, origin, confidence, valid_from, valid_to, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                e.source_id,
                e.target_id,
                e.predicate,
                json_to_string(&e.properties),
                e.origin.as_str(),
                e.confidence,
                e.valid_from,
                e.valid_to,
                e.created_at,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    async fn get_edges_touching(&self, object_id: i64) -> Result<Vec<KnowledgeEdge>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT * FROM knowledge_edges WHERE source_id = ?1 OR target_id = ?1 \
             ORDER BY valid_from ASC NULLS LAST",
        )?;
        let rows = stmt.query_map(params![object_id], row_to_edge)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    async fn update_edge_target(&self, edge_id: i64, new_target_id: i64) -> Result<usize> {
        let conn = self.conn.lock().await;
        let n = conn.execute(
            "UPDATE knowledge_edges SET target_id = ?1 WHERE id = ?2",
            params![new_target_id, edge_id],
        )?;
        Ok(n)
    }

    async fn create_evidence(&self, e: &Evidence) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO evidence (doc_id, chapter_id, start_offset, end_offset, content, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![e.doc_id, e.chapter_id, e.start_offset, e.end_offset, e.content, e.created_at],
        )?;
        Ok(conn.last_insert_rowid())
    }

    async fn link_evidence(
        &self,
        source_type: EvidenceSourceType,
        source_id: i64,
        evidence_id: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        // INSERT OR IGNORE makes this idempotent against the UNIQUE
        // (source_type, source_id, evidence_id) constraint.
        conn.execute(
            "INSERT OR IGNORE INTO knowledge_evidence (source_type, source_id, evidence_id)
             VALUES (?1, ?2, ?3)",
            params![source_type.as_str(), source_id, evidence_id],
        )?;
        Ok(())
    }

    async fn get_evidence_for(
        &self,
        source_type: EvidenceSourceType,
        source_id: i64,
    ) -> Result<Vec<Evidence>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT e.* FROM evidence e
             JOIN knowledge_evidence ke ON ke.evidence_id = e.id
             WHERE ke.source_type = ?1 AND ke.source_id = ?2
             ORDER BY e.id ASC",
        )?;
        let rows = stmt.query_map(params![source_type.as_str(), source_id], row_to_evidence)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    async fn get_evidence_for_many(
        &self,
        source_type: EvidenceSourceType,
        source_ids: &[i64],
    ) -> Result<Vec<Evidence>> {
        self.get_evidence_for_many_query(source_type, source_ids)
            .await
    }

    async fn create_mention(&self, m: &Mention) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO mentions (object_id, chapter_id, start_offset, end_offset, alias_used, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![m.object_id, m.chapter_id, m.start_offset, m.end_offset, m.alias_used, m.confidence],
        )?;
        Ok(conn.last_insert_rowid())
    }

    async fn get_mentions_for_object(&self, object_id: i64) -> Result<Vec<Mention>> {
        let conn = self.conn.lock().await;
        let mut stmt =
            conn.prepare("SELECT * FROM mentions WHERE object_id = ?1 ORDER BY chapter_id ASC")?;
        let rows = stmt.query_map(params![object_id], row_to_mention)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    async fn list_documents(&self) -> Result<Vec<Document>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT * FROM documents ORDER BY id ASC")?;
        let rows = stmt.query_map([], row_to_document)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    async fn list_objects_by_document(&self, doc_id: i64) -> Result<Vec<KnowledgeObject>> {
        let conn = self.conn.lock().await;
        let mut stmt =
            conn.prepare("SELECT * FROM knowledge_objects WHERE doc_id = ?1 ORDER BY id ASC")?;
        let rows = stmt.query_map(params![doc_id], row_to_object)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    async fn list_edges_by_document(&self, doc_id: i64) -> Result<Vec<KnowledgeEdge>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT ke.* FROM knowledge_edges ke \
             JOIN knowledge_objects src ON ke.source_id = src.id \
             WHERE src.doc_id = ?1 ORDER BY ke.id ASC",
        )?;
        let rows = stmt.query_map(params![doc_id], row_to_edge)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    async fn list_evidence_by_document(&self, doc_id: i64) -> Result<Vec<Evidence>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT * FROM evidence WHERE doc_id = ?1 ORDER BY id ASC")?;
        let rows = stmt.query_map(params![doc_id], row_to_evidence)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    async fn list_evidence_links(&self) -> Result<Vec<KnowledgeEvidenceLink>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, source_type, source_id, evidence_id FROM knowledge_evidence ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(KnowledgeEvidenceLink {
                id: row.get("id")?,
                source_type: std::str::FromStr::from_str(&row.get::<_, String>("source_type")?)
                    .unwrap_or(EvidenceSourceType::Object),
                source_id: row.get("source_id")?,
                evidence_id: row.get("evidence_id")?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    async fn search_objects(
        &self,
        name_contains: Option<&str>,
        object_type: Option<&str>,
        property_contains: Option<&str>,
        doc_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<KnowledgeObject>> {
        let conn = self.conn.lock().await;
        // Clamp like search_evidence: `usize::MAX as i64` becomes -1, which
        // SQLite treats as "no limit" and materializes the whole table.
        let limit = limit.min(10_000) as i64;
        // Escape LIKE wildcards so `%`/`_` in the user's query match
        // literally instead of acting as wildcards (mirrors search_evidence).
        let name_like = name_contains.map(|n| {
            n.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        });
        let prop_like = property_contains.map(|p| {
            p.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        });
        let mut stmt = conn.prepare(
            "SELECT * FROM knowledge_objects \
             WHERE (?1 IS NULL OR name LIKE '%' || ?1 || '%' ESCAPE '\\') \
               AND (?2 IS NULL OR object_type = ?2) \
               AND (?3 IS NULL OR properties LIKE '%' || ?3 || '%' ESCAPE '\\') \
               AND (?4 IS NULL OR doc_id = ?4) \
             ORDER BY id ASC LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![name_like, object_type, prop_like, doc_id, limit,],
            row_to_object,
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    async fn graph_counts(&self) -> Result<GraphCounts> {
        let conn = self.conn.lock().await;
        let documents =
            conn.query_row("SELECT COUNT(*) FROM documents", [], |r| r.get::<_, i64>(0))?;
        let objects = conn.query_row("SELECT COUNT(*) FROM knowledge_objects", [], |r| {
            r.get::<_, i64>(0)
        })?;
        let edges = conn.query_row("SELECT COUNT(*) FROM knowledge_edges", [], |r| {
            r.get::<_, i64>(0)
        })?;
        let evidence =
            conn.query_row("SELECT COUNT(*) FROM evidence", [], |r| r.get::<_, i64>(0))?;
        Ok(GraphCounts {
            documents: documents as usize,
            objects: objects as usize,
            edges: edges as usize,
            evidence: evidence as usize,
        })
    }

    async fn find_world_entity(&self, name: &str) -> Result<Option<WorldEntity>> {
        self.find_world_entity_row(name).await
    }

    async fn upsert_world_entity(
        &self,
        name: &str,
        entity_type: &str,
        importance: f64,
    ) -> Result<i64> {
        self.upsert_world_entity_row(name, entity_type, importance)
            .await
    }

    async fn upsert_world_profile(
        &self,
        entity_id: i64,
        key: &str,
        value: &str,
        confidence: f64,
        evidence_id: Option<i64>,
    ) -> Result<()> {
        self.upsert_world_profile_row(entity_id, key, value, confidence, evidence_id)
            .await
    }

    async fn upsert_world_relation(
        &self,
        source_id: i64,
        target_id: i64,
        relation_type: &str,
        confidence: f64,
    ) -> Result<()> {
        self.upsert_world_relation_row(source_id, target_id, relation_type, confidence)
            .await
    }

    async fn upsert_world_event(
        &self,
        title: &str,
        event_type: &str,
        timestamp: Option<i32>,
        location: Option<&str>,
        description: &str,
        importance: f64,
        start_offset: Option<i64>,
        end_offset: Option<i64>,
    ) -> Result<i64> {
        self.upsert_world_event_row(
            title,
            event_type,
            timestamp,
            location,
            description,
            importance,
            start_offset,
            end_offset,
        )
        .await
    }

    async fn link_event_participant(
        &self,
        event_id: i64,
        entity_name: &str,
        role: &str,
    ) -> Result<()> {
        self.link_event_participant_row(event_id, entity_name, role)
            .await
    }

    async fn list_world_events(&self) -> Result<Vec<WorldEvent>> {
        self.list_world_events_row().await
    }

    async fn upsert_world_state(
        &self,
        entity_name: &str,
        slot: &str,
        value: &str,
        chapter: Option<i32>,
        event_id: Option<i64>,
        start_offset: Option<i64>,
        end_offset: Option<i64>,
        confidence: f64,
    ) -> Result<i64> {
        self.upsert_world_state_row(
            entity_name,
            slot,
            value,
            chapter,
            event_id,
            start_offset,
            end_offset,
            confidence,
        )
        .await
    }

    async fn list_world_states(&self, entity_name: Option<&str>) -> Result<Vec<WorldState>> {
        self.list_world_states_row(entity_name).await
    }

    async fn list_world_entities(&self) -> Result<Vec<WorldEntity>> {
        self.list_world_entities_row().await
    }

    async fn list_world_profiles(&self) -> Result<Vec<WorldProfile>> {
        self.list_world_profiles_row().await
    }

    async fn list_world_relations(&self) -> Result<Vec<WorldRelation>> {
        self.list_world_relations_row().await
    }

    async fn create_run(&self, r: &CompilerRun) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO compiler_runs (doc_id, version, started_at, finished_at, status, statistics)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                r.doc_id,
                r.version,
                r.started_at,
                r.finished_at,
                r.status,
                json_to_string(r.statistics.as_ref().unwrap_or(&serde_json::Value::Null)),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    async fn finish_run(
        &self,
        id: i64,
        status: &str,
        statistics: &serde_json::Value,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        let affected = conn.execute(
            "UPDATE compiler_runs SET status = ?2, finished_at = ?3, statistics = ?4 WHERE id = ?1",
            params![
                id,
                status,
                chrono::Utc::now().timestamp(),
                json_to_string(statistics)
            ],
        )?;
        if affected == 0 {
            return Err(StorageError::NotFound(format!("compiler_run {id}")).into());
        }
        Ok(())
    }

    async fn inspect_entity(
        &self,
        name: &str,
        doc_title: Option<&str>,
    ) -> Result<Option<InspectEntityResult>> {
        self.inspect_entity_query(name, doc_title).await
    }

    async fn entity_timeline(
        &self,
        name: &str,
        doc_title: Option<&str>,
    ) -> Result<Vec<TimelineEntry>> {
        self.entity_timeline_query(name, doc_title).await
    }

    async fn relation_graph(
        &self,
        name: &str,
        depth: usize,
        doc_title: Option<&str>,
    ) -> Result<Option<RelationGraphResult>> {
        self.relation_graph_query(name, depth, doc_title).await
    }

    async fn search_evidence(
        &self,
        query: &str,
        doc_title: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EvidenceHit>> {
        self.search_evidence_query(query, doc_title, limit).await
    }
}

impl SQLiteKnowledgeStore {
    /// Resolve an optional document-title filter to an optional doc_id.
    /// Returns `Ok(None)` when no filter is given (match across all docs).
    async fn resolve_doc_id(&self, doc_title: Option<&str>) -> Result<Option<i64>> {
        match doc_title {
            Some(t) => match self.find_document_by_title(t).await? {
                Some(d) => Ok(Some(d.id)),
                None => Err(Error::NotFound(format!("document `{t}`"))),
            },
            None => Ok(None),
        }
    }

    /// Resolve a batch of mentions' `chapter_id` (surrogate FK) to their
    /// narrative `chapter_no`. Used by `inspect_entity` so that lifecycle
    /// `first_seen`/`last_seen` report the chapter number a reader expects
    /// (e.g. 41), not the row id (e.g. 161) — see NEW-K1.
    ///
    /// Mentions whose `chapter_id` has no matching chapter row (stale data)
    /// are silently dropped from the result.
    async fn resolve_chapter_nos(&self, mentions: &[Mention]) -> Result<Vec<i32>> {
        if mentions.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<i64> = mentions.iter().map(|m| m.chapter_id).collect();
        let conn = self.conn.lock().await;
        // Build a parameterized `IN (?, ?, ...)` clause.
        let placeholders: Vec<&str> = ids.iter().map(|_| "?").collect();
        let sql = format!(
            "SELECT id, chapter_no FROM chapters WHERE id IN ({})",
            placeholders.join(", ")
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = ids
            .iter()
            .map(|id| Box::new(*id) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        let rows = stmt.query_map(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i32>(1)?)),
        )?;
        let mut id_to_no: HashMap<i64, i32> = HashMap::new();
        for r in rows {
            let (id, no) = r?;
            id_to_no.insert(id, no);
        }
        Ok(mentions
            .iter()
            .filter_map(|m| id_to_no.get(&m.chapter_id).copied())
            .collect())
    }
}
