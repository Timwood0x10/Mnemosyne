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
use rusqlite::{Connection, params};
use tokio::sync::Mutex;

use crate::error::{Error, Result, StorageError};
use crate::storage::KNOWLEDGE_SCHEMA;

use super::{
    Chapter, CompilerRun, Document, EntityProfileEntry, Evidence, EvidenceHit, EvidenceSourceType,
    GraphEdge, GraphNode, InspectEntityResult, KnowledgeEdge, KnowledgeObject, Mention, ObjectType,
    Origin, RelationGraphResult, TimelineEntry,
};

/// Convert a rusqlite row into a [`Document`].
fn row_to_document(row: &rusqlite::Row) -> rusqlite::Result<Document> {
    Ok(Document {
        id: row.get("id")?,
        title: row.get("title")?,
        author: row.get("author")?,
        doc_type: row.get("doc_type")?,
        created_at: row.get("created_at")?,
    })
}

/// Convert a rusqlite row into a [`Chapter`].
fn row_to_chapter(row: &rusqlite::Row) -> rusqlite::Result<Chapter> {
    Ok(Chapter {
        id: row.get("id")?,
        doc_id: row.get("doc_id")?,
        chapter_no: row.get("chapter_no")?,
        title: row.get("title")?,
        content: row.get("content")?,
        start_offset: row.get("start_offset")?,
        end_offset: row.get("end_offset")?,
    })
}

/// Convert a rusqlite row into a [`KnowledgeObject`], parsing the JSON
/// `properties` bag defensively (bad JSON → empty object).
fn row_to_object(row: &rusqlite::Row) -> rusqlite::Result<KnowledgeObject> {
    let type_str: String = row.get("object_type")?;
    let props_str: String = row.get("properties").unwrap_or_default();
    let object_type = type_str.parse::<ObjectType>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::<std::io::Error>::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;
    Ok(KnowledgeObject {
        id: row.get("id")?,
        doc_id: row.get("doc_id")?,
        object_type,
        name: row.get("name")?,
        properties: serde_json::from_str(&props_str)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
        confidence: row.get("confidence")?,
        created_at: row.get("created_at")?,
    })
}

/// Convert a rusqlite row into a [`KnowledgeEdge`].
fn row_to_edge(row: &rusqlite::Row) -> rusqlite::Result<KnowledgeEdge> {
    let origin_str: String = row.get("origin")?;
    let props_str: String = row.get("properties").unwrap_or_default();
    let origin = origin_str.parse::<Origin>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::<std::io::Error>::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;
    Ok(KnowledgeEdge {
        id: row.get("id")?,
        source_id: row.get("source_id")?,
        target_id: row.get("target_id")?,
        predicate: row.get("predicate")?,
        properties: serde_json::from_str(&props_str)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
        origin,
        confidence: row.get("confidence")?,
        valid_from: row.get("valid_from")?,
        valid_to: row.get("valid_to")?,
        created_at: row.get("created_at")?,
    })
}

/// Convert a rusqlite row into an [`Evidence`].
fn row_to_evidence(row: &rusqlite::Row) -> rusqlite::Result<Evidence> {
    Ok(Evidence {
        id: row.get("id")?,
        doc_id: row.get("doc_id")?,
        chapter_id: row.get("chapter_id")?,
        start_offset: row.get("start_offset")?,
        end_offset: row.get("end_offset")?,
        content: row.get("content")?,
        created_at: row.get("created_at")?,
    })
}

/// Convert a rusqlite row into a [`Mention`].
fn row_to_mention(row: &rusqlite::Row) -> rusqlite::Result<Mention> {
    Ok(Mention {
        id: row.get("id")?,
        object_id: row.get("object_id")?,
        chapter_id: row.get("chapter_id")?,
        start_offset: row.get("start_offset")?,
        end_offset: row.get("end_offset")?,
        alias_used: row.get("alias_used")?,
        confidence: row.get("confidence")?,
    })
}

/// Serialize a `serde_json::Value` for the `properties`/`statistics` JSON
/// columns. A null value is stored as the empty object so the column default
/// semantics are preserved.
fn json_to_string(v: &serde_json::Value) -> String {
    if v.is_null() {
        "{}".to_string()
    } else {
        serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Backend-agnostic knowledge store contract.
///
/// All create methods return the new row's `id` so callers can chain inserts
/// (object → edge → evidence → link) without re-querying.
#[async_trait]
pub trait KnowledgeStore: Send + Sync {
    // ── documents / chapters ───────────────────────────────────
    async fn create_document(&self, d: &Document) -> Result<i64>;
    async fn find_document_by_title(&self, title: &str) -> Result<Option<Document>>;
    async fn create_chapter(&self, c: &Chapter) -> Result<i64>;
    async fn get_chapter_by_no(&self, doc_id: i64, chapter_no: i32) -> Result<Option<Chapter>>;

    // ── objects ───────────────────────────────────────────────
    async fn create_object(&self, o: &KnowledgeObject) -> Result<i64>;
    async fn get_object(&self, id: i64) -> Result<Option<KnowledgeObject>>;
    async fn find_object_by_name(
        &self,
        name: &str,
        doc_id: Option<i64>,
    ) -> Result<Option<KnowledgeObject>>;

    // ── edges ─────────────────────────────────────────────────
    async fn create_edge(&self, e: &KnowledgeEdge) -> Result<i64>;
    async fn get_edges_touching(&self, object_id: i64) -> Result<Vec<KnowledgeEdge>>;
    /// Re-target an edge to point at `new_target_id`. Used by the
    /// `correct_relation` MCP tool to fix misattributed relations in place.
    async fn update_edge_target(&self, edge_id: i64, new_target_id: i64) -> Result<()>;

    // ── evidence + links ──────────────────────────────────────
    async fn create_evidence(&self, e: &Evidence) -> Result<i64>;
    /// Idempotent: inserting a duplicate (source, evidence) pair is a no-op.
    async fn link_evidence(
        &self,
        source_type: EvidenceSourceType,
        source_id: i64,
        evidence_id: i64,
    ) -> Result<()>;
    async fn get_evidence_for(
        &self,
        source_type: EvidenceSourceType,
        source_id: i64,
    ) -> Result<Vec<Evidence>>;

    // ── mentions ─────────────────────────────────────────────
    async fn create_mention(&self, m: &Mention) -> Result<i64>;
    async fn get_mentions_for_object(&self, object_id: i64) -> Result<Vec<Mention>>;

    // ── compiler runs ────────────────────────────────────────
    async fn create_run(&self, r: &CompilerRun) -> Result<i64>;
    async fn finish_run(&self, id: i64, status: &str, statistics: &serde_json::Value)
    -> Result<()>;

    // ── high-level queries (dev_guide §5) ─────────────────────
    async fn inspect_entity(
        &self,
        name: &str,
        doc_title: Option<&str>,
    ) -> Result<Option<InspectEntityResult>>;
    async fn entity_timeline(
        &self,
        name: &str,
        doc_title: Option<&str>,
    ) -> Result<Vec<TimelineEntry>>;
    async fn relation_graph(
        &self,
        name: &str,
        depth: usize,
        doc_title: Option<&str>,
    ) -> Result<Option<RelationGraphResult>>;
    async fn search_evidence(
        &self,
        query: &str,
        doc_title: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EvidenceHit>>;
}

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
        let conn = self.conn.lock().await;
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             DELETE FROM knowledge_evidence;
             DELETE FROM mentions;
             DELETE FROM knowledge_edges;
             DELETE FROM evidence;
             DELETE FROM knowledge_objects;
             DELETE FROM compiler_runs;
             DELETE FROM chapters;
             DELETE FROM documents;
             PRAGMA foreign_keys = ON;",
        )?;
        Ok(())
    }

    /// Drop all rows belonging to a single document (chapters, objects, edges,
    /// evidence, mentions, compiler_runs). Used by the migrator to make
    /// re-migrating one novel a clean rebuild without touching other novels'
    /// data. FK enforcement is disabled during the wipe for the same reason as
    /// [`clear_all`].
    pub async fn clear_for_document(&self, doc_id: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch(&format!(
            "PRAGMA foreign_keys = OFF;
             DELETE FROM knowledge_evidence
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
             DELETE FROM chapters WHERE doc_id = {doc_id};
             PRAGMA foreign_keys = ON;",
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

    async fn update_edge_target(&self, edge_id: i64, new_target_id: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE knowledge_edges SET target_id = ?1 WHERE id = ?2",
            params![new_target_id, edge_id],
        )?;
        Ok(())
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
        let doc_id = self.resolve_doc_id(doc_title).await?;
        let object = match self.find_object_by_name(name, doc_id).await? {
            Some(o) => o,
            None => return Ok(None),
        };

        let edges = self.get_edges_touching(object.id).await?;

        // Split edges: `participated_in` edges point at event objects; the rest
        // are entity↔entity relations. Dedup event ids so duplicate edges (e.g.
        // from a non-idempotent re-migration) don't inflate the events list and
        // `event_count`.
        let mut event_ids: HashSet<i64> = HashSet::new();
        let mut relations: Vec<KnowledgeEdge> = Vec::new();
        for e in &edges {
            if e.predicate == "participated_in" {
                // The entity is the source; the event is the target. Guard
                // against inverted data by picking the non-entity endpoint.
                let target = if e.source_id == object.id {
                    e.target_id
                } else {
                    e.source_id
                };
                event_ids.insert(target);
            } else {
                relations.push(e.clone());
            }
        }

        let mut events: Vec<KnowledgeObject> = Vec::new();
        for eid in &event_ids {
            if let Some(ev) = self.get_object(*eid).await? {
                events.push(ev);
            }
        }

        // Evidences: union of object evidence + each edge's evidence, deduped.
        let mut seen_ev: HashSet<i64> = HashSet::new();
        let mut evidences: Vec<Evidence> = Vec::new();
        for ev in self
            .get_evidence_for(EvidenceSourceType::Object, object.id)
            .await?
        {
            if seen_ev.insert(ev.id) {
                evidences.push(ev);
            }
        }
        for e in &edges {
            for ev in self
                .get_evidence_for(EvidenceSourceType::Edge, e.id)
                .await?
            {
                if seen_ev.insert(ev.id) {
                    evidences.push(ev);
                }
            }
        }

        let mentions = self.get_mentions_for_object(object.id).await?;
        let event_count = events.len();

        // Derive lifecycle from mentions (first/last chapter the entity appears
        // in) and events (death chapter). Previously these were hardcoded to
        // `None` even though the data was available.
        //
        // NEW-K1 fix: `Mention.chapter_id` is a surrogate FK into `chapters.id`,
        // NOT the narrative chapter number. For the first migrated document
        // these align (both start at 1), but for the second document the
        // chapter IDs are offset by however many chapters the first document
        // had. We resolve to `chapter_no` via a bulk lookup so the lifecycle
        // reports the narrative chapter (e.g. 41 for 赵云 rescuing 阿斗), not
        // the row id (e.g. 161).
        let chapter_nos = self.resolve_chapter_nos(&mentions).await?;
        let first_seen = chapter_nos.iter().copied().min();
        let last_seen = chapter_nos.iter().copied().max();
        let death_chapter = events.iter().find_map(|ev| {
            let name = ev.name.as_str();
            let desc = ev
                .properties
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // Check both event name and description for death keywords.
            // The old code only checked the name, missing deaths described
            // in the `properties.description` field (NEW-K8).
            let death_kws = [
                "战死", "去世", "身亡", "阵亡", "死亡", "死", "病逝", "病故", "殒命", "毙命",
                "驾崩", "圆寂", "陨落", "卒",
            ];
            if death_kws
                .iter()
                .any(|k| name.contains(k) || desc.contains(k))
            {
                ev.properties
                    .get("chapter")
                    .and_then(|v| v.as_i64())
                    .map(|c| c as i32)
            } else {
                None
            }
        });

        // Populate profile entries from the object's properties JSON (clothing,
        // personality, description, aliases) instead of always returning empty.
        let mut profile: Vec<EntityProfileEntry> = Vec::new();
        if let Some(props) = object.properties.as_object() {
            for key in ["description", "personality", "clothing", "aliases"] {
                if let Some(val) = props.get(key) {
                    let value = match val {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    if !value.is_empty() && value != "null" {
                        profile.push(EntityProfileEntry {
                            key: key.to_string(),
                            value,
                            confidence: object.confidence,
                        });
                    }
                }
            }
        }

        Ok(Some(InspectEntityResult {
            object,
            profile,
            events,
            relations,
            evidences,
            mentions,
            lifecycle: crate::knowledge::EntityLifecycle {
                first_seen,
                last_seen,
                death_chapter,
                event_count,
            },
            character_arc: None,
            // The store layer is link-unaware; the MCP `inspect_entity` tool
            // populates this field from the attached EntityLinker so the
            // cross-source aliases ride on the same response payload
            // (external-knowledge-plan §D3).
            external_aliases: Vec::new(),
        }))
    }

    async fn entity_timeline(
        &self,
        name: &str,
        doc_title: Option<&str>,
    ) -> Result<Vec<TimelineEntry>> {
        let doc_id = self.resolve_doc_id(doc_title).await?;
        let object = match self.find_object_by_name(name, doc_id).await? {
            Some(o) => o,
            None => return Ok(Vec::new()),
        };
        let edges = self.get_edges_touching(object.id).await?;

        // Materialize the "other endpoint" object for each edge so we can report
        // its name and, for participated_in, the event name.
        let mut neighbor_ids: HashSet<i64> = HashSet::new();
        for e in &edges {
            let other = if e.source_id == object.id {
                e.target_id
            } else {
                e.source_id
            };
            neighbor_ids.insert(other);
        }
        let mut neighbors: HashMap<i64, KnowledgeObject> = HashMap::new();
        for nid in &neighbor_ids {
            if let Some(n) = self.get_object(*nid).await? {
                neighbors.insert(*nid, n);
            }
        }

        let mut entries: Vec<TimelineEntry> = Vec::new();
        for e in &edges {
            let other_id = if e.source_id == object.id {
                e.target_id
            } else {
                e.source_id
            };
            let target = neighbors
                .get(&other_id)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| format!("#{other_id}"));
            // For participated_in the "event" label is the event object's name;
            // for other relations it is the predicate + target summary.
            let event_label = if e.predicate == "participated_in" {
                target.clone()
            } else {
                format!("{} → {}", e.predicate, target)
            };
            entries.push(TimelineEntry {
                chapter: e.valid_from.unwrap_or(0),
                event: event_label,
                predicate: e.predicate.clone(),
                target,
            });
        }
        // Stable order by chapter; ties keep insertion (valid_from ASC) order.
        entries.sort_by_key(|t| t.chapter);
        Ok(entries)
    }

    async fn relation_graph(
        &self,
        name: &str,
        depth: usize,
        doc_title: Option<&str>,
    ) -> Result<Option<RelationGraphResult>> {
        let doc_id = self.resolve_doc_id(doc_title).await?;
        let root = match self.find_object_by_name(name, doc_id).await? {
            Some(o) => o,
            None => return Ok(None),
        };

        // BFS up to `depth` hops to determine the reachable node set. Edges
        // are treated as undirected for neighborhood expansion (a relation is
        // bidirectionally observable from either endpoint). `0..depth` (not
        // `0..=depth`) so depth=N traverses exactly N hops: depth=1 yields
        // direct neighbors only, not their neighbors too.
        let mut frontier: Vec<i64> = vec![root.id];
        let mut reachable: HashSet<i64> = HashSet::new();
        reachable.insert(root.id);
        for _ in 0..depth {
            let mut next_frontier: Vec<i64> = Vec::new();
            for &oid in &frontier {
                for e in self.get_edges_touching(oid).await? {
                    let other = if e.source_id == oid {
                        e.target_id
                    } else {
                        e.source_id
                    };
                    if reachable.insert(other) {
                        next_frontier.push(other);
                    }
                }
            }
            if next_frontier.is_empty() {
                break;
            }
            frontier = next_frontier;
        }

        // Build the induced subgraph on `reachable` with petgraph. `DiGraph`
        // (not `DiGraphMap`) preserves parallel edges: two relations between
        // the same pair with different predicates (e.g. 刘备→关羽 "结义" and
        // "trusts") both survive instead of collapsing to last-wins. Edges are
        // deduped by `KnowledgeEdge::id` (each is returned once per endpoint),
        // and only edges with both endpoints inside `reachable` are kept so the
        // subgraph stays within the requested depth.
        let mut graph: petgraph::graph::DiGraph<i64, GraphEdge> = petgraph::graph::DiGraph::new();
        let mut node_idx: HashMap<i64, petgraph::graph::NodeIndex> = HashMap::new();
        for &oid in &reachable {
            node_idx.insert(oid, graph.add_node(oid));
        }
        let mut seen_edge_ids: HashSet<i64> = HashSet::new();
        for &oid in &reachable {
            for e in self.get_edges_touching(oid).await? {
                if !seen_edge_ids.insert(e.id) {
                    continue;
                }
                if !reachable.contains(&e.source_id) || !reachable.contains(&e.target_id) {
                    continue;
                }
                let (s, t) = (node_idx[&e.source_id], node_idx[&e.target_id]);
                graph.add_edge(
                    s,
                    t,
                    GraphEdge {
                        source_id: e.source_id,
                        target_id: e.target_id,
                        predicate: e.predicate.clone(),
                        valid_from: e.valid_from,
                        valid_to: e.valid_to,
                        confidence: e.confidence,
                    },
                );
            }
        }

        // Materialize nodes (name + type lookup) and edges.
        let mut nodes: Vec<GraphNode> = Vec::new();
        for idx in graph.node_indices() {
            let oid = graph[idx];
            let obj = if oid == root.id {
                root.clone()
            } else {
                match self.get_object(oid).await? {
                    Some(o) => o,
                    None => continue,
                }
            };
            nodes.push(GraphNode {
                id: obj.id,
                name: obj.name,
                object_type: obj.object_type,
            });
        }
        let edges: Vec<GraphEdge> = graph
            .edge_indices()
            .filter_map(|idx| graph.edge_weight(idx).cloned())
            .collect();

        Ok(Some(RelationGraphResult { nodes, edges }))
    }

    async fn search_evidence(
        &self,
        query: &str,
        doc_title: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EvidenceHit>> {
        let conn = self.conn.lock().await;
        // Clamp to avoid `usize::MAX as i64` overflow (which becomes -1 and is
        // treated as "no limit" by SQLite) and to bound memory use.
        let limit = limit.min(10_000) as i64;
        let like = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match doc_title {
            Some(t) => (
                "SELECT e.content, c.chapter_no, d.title
                 FROM evidence e
                 JOIN chapters c ON c.id = e.chapter_id
                 JOIN documents d ON d.id = e.doc_id
                 WHERE e.content LIKE ?1 ESCAPE '\\' AND d.title = ?2
                 ORDER BY e.id ASC LIMIT ?3"
                    .to_string(),
                vec![
                    Box::new(like) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(t.to_string()),
                    Box::new(limit),
                ],
            ),
            None => (
                "SELECT e.content, c.chapter_no, d.title
                 FROM evidence e
                 JOIN chapters c ON c.id = e.chapter_id
                 JOIN documents d ON d.id = e.doc_id
                 WHERE e.content LIKE ?1 ESCAPE '\\'
                 ORDER BY e.id ASC LIMIT ?2"
                    .to_string(),
                vec![
                    Box::new(like) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(limit),
                ],
            ),
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(params_vec.iter().map(|p| p.as_ref())),
            |row| {
                let content: String = row.get(0)?;
                let chapter: i32 = row.get(1)?;
                let doc: String = row.get(2)?;
                Ok(EvidenceHit {
                    text: content,
                    chapter,
                    doc,
                    // Observed text evidence has no separate confidence score;
                    // it is authoritative by construction.
                    confidence: 1.0,
                })
            },
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now_ts() -> i64 {
        chrono::Utc::now().timestamp()
    }

    async fn fresh() -> SQLiteKnowledgeStore {
        SQLiteKnowledgeStore::open_in_memory().await.expect("open")
    }

    async fn seed_doc(store: &SQLiteKnowledgeStore, title: &str) -> i64 {
        store
            .create_document(&Document {
                id: 0,
                title: title.to_string(),
                author: None,
                doc_type: Some("novel".into()),
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
        assert_eq!(got.id, cid);
        assert_eq!(got.chapter_no, 3);
    }

    /// Objective: Verify object CRUD, properties JSON round-trip, and
    /// name-based lookup scoped to a doc.
    /// Invariants: properties survive serialize/parse; find_object_by_name
    /// returns None for a name that does not exist.
    #[tokio::test]
    async fn object_round_trip_and_lookup() {
        let store = fresh().await;
        let did = seed_doc(&store, "水浒传").await;
        let props = json!({"aliases": ["及时雨"], "importance": 0.8});
        let oid = seed_person(&store, did, "宋江", props.clone()).await;

        let got = store.get_object(oid).await.expect("get").expect("exists");
        assert_eq!(got.name, "宋江");
        assert_eq!(got.properties, props, "properties JSON must round-trip");

        let found = store
            .find_object_by_name("宋江", Some(did))
            .await
            .expect("find")
            .expect("found by name");
        assert_eq!(found.id, oid);

        let missing = store
            .find_object_by_name("不存在的角色", Some(did))
            .await
            .expect("find missing");
        assert!(missing.is_none(), "unknown name must return None");
    }

    /// Objective: Verify edge creation + `get_edges_touching` returns both
    /// outgoing and incoming edges for an object.
    /// Invariants: A↔B edge is returned when querying either endpoint.
    #[tokio::test]
    async fn edge_touches_both_endpoints() {
        let store = fresh().await;
        let did = seed_doc(&store, "三国演义").await;
        let a = seed_person(&store, did, "刘备", json!({})).await;
        let b = seed_person(&store, did, "关羽", json!({})).await;
        store
            .create_edge(&KnowledgeEdge {
                id: 0,
                source_id: a,
                target_id: b,
                predicate: "结义".into(),
                properties: json!({}),
                origin: Origin::Observed,
                confidence: 0.9,
                valid_from: Some(1),
                valid_to: None,
                created_at: now_ts(),
            })
            .await
            .expect("create edge");

        let from_a = store.get_edges_touching(a).await.expect("edges a");
        let from_b = store.get_edges_touching(b).await.expect("edges b");
        assert_eq!(from_a.len(), 1, "source endpoint sees the edge");
        assert_eq!(from_b.len(), 1, "target endpoint also sees the edge");
        assert_eq!(from_a[0].predicate, "结义");
        assert_eq!(from_a[0].valid_from, Some(1));
    }

    /// Objective: Verify evidence linking is idempotent (UNIQUE constraint)
    /// and that get_evidence_for returns linked rows.
    /// Invariants: linking the same (source, evidence) twice does not error
    /// and does not duplicate the row returned.
    #[tokio::test]
    async fn evidence_link_is_idempotent() {
        let store = fresh().await;
        let did = seed_doc(&store, "三国演义").await;
        let cid = seed_chapter(&store, did, 1).await;
        let oid = seed_person(&store, did, "赵云", json!({})).await;
        let eid = store
            .create_evidence(&Evidence {
                id: 0,
                doc_id: did,
                chapter_id: cid,
                start_offset: Some(0),
                end_offset: Some(5),
                content: "赵云单骑救主".into(),
                created_at: now_ts(),
            })
            .await
            .expect("create evidence");

        store
            .link_evidence(EvidenceSourceType::Object, oid, eid)
            .await
            .expect("link once");
        // Second link must be a no-op, not an error.
        store
            .link_evidence(EvidenceSourceType::Object, oid, eid)
            .await
            .expect("link twice is idempotent");

        let evs = store
            .get_evidence_for(EvidenceSourceType::Object, oid)
            .await
            .expect("get evidence");
        assert_eq!(evs.len(), 1, "duplicate link must not duplicate rows");
        assert_eq!(evs[0].content, "赵云单骑救主");
    }

    /// Objective: Verify `inspect_entity` returns Object + Edges + Evidence
    /// and splits participated_in edges into the events list.
    /// Invariants: a person with one relation + one participated_in event +
    /// one evidence yields 1 relation, 1 event, 1 evidence.
    #[tokio::test]
    async fn inspect_entity_returns_full_picture() {
        let store = fresh().await;
        let did = seed_doc(&store, "三国演义").await;
        let zhaoyun = seed_person(&store, did, "赵云", json!({})).await;
        let liubei = seed_person(&store, did, "刘备", json!({})).await;
        let event_id = store
            .create_object(&KnowledgeObject {
                id: 0,
                doc_id: did,
                object_type: ObjectType::Event,
                name: "单骑救主".into(),
                properties: json!({}),
                confidence: 1.0,
                created_at: now_ts(),
            })
            .await
            .expect("create event object");

        let cid = seed_chapter(&store, did, 41).await;
        let eid = store
            .create_evidence(&Evidence {
                id: 0,
                doc_id: did,
                chapter_id: cid,
                start_offset: None,
                end_offset: None,
                content: "赵云怀抱阿斗，杀透重围".into(),
                created_at: now_ts(),
            })
            .await
            .expect("create evidence");

        let trust_edge = KnowledgeEdge {
            id: 0,
            source_id: liubei,
            target_id: zhaoyun,
            predicate: "trusts".into(),
            properties: json!({}),
            origin: Origin::Observed,
            confidence: 0.7,
            valid_from: Some(41),
            valid_to: None,
            created_at: now_ts(),
        };
        store
            .create_edge(&trust_edge)
            .await
            .expect("create trust edge");
        let part_edge = KnowledgeEdge {
            id: 0,
            source_id: zhaoyun,
            target_id: event_id,
            predicate: "participated_in".into(),
            properties: json!({}),
            origin: Origin::Observed,
            confidence: 1.0,
            valid_from: Some(41),
            valid_to: None,
            created_at: now_ts(),
        };
        let part_edge_id = store
            .create_edge(&part_edge)
            .await
            .expect("create part edge");
        store
            .link_evidence(EvidenceSourceType::Object, zhaoyun, eid)
            .await
            .expect("link object evidence");
        store
            .link_evidence(EvidenceSourceType::Edge, part_edge_id, eid)
            .await
            .expect("link edge evidence");

        let result = store
            .inspect_entity("赵云", Some("三国演义"))
            .await
            .expect("inspect")
            .expect("entity found");
        assert_eq!(result.object.name, "赵云");
        assert_eq!(result.relations.len(), 1, "one person↔person relation");
        assert_eq!(result.events.len(), 1, "one participated_in event");
        assert_eq!(result.events[0].name, "单骑救主");
        assert_eq!(
            result.evidences.len(),
            1,
            "evidence deduped across object+edge links"
        );
        assert!(result.evidences.iter().any(|e| e.content.contains("阿斗")));
    }

    /// Objective: Verify `inspect_entity` returns None for an unknown entity
    /// rather than erroring.
    /// Invariants: unknown name → Ok(None); unknown doc title → Err.
    #[tokio::test]
    async fn inspect_entity_missing() {
        let store = fresh().await;
        let did = seed_doc(&store, "三国演义").await;
        seed_person(&store, did, "赵云", json!({})).await;
        assert!(
            store
                .inspect_entity("不存在", Some("三国演义"))
                .await
                .expect("unknown entity")
                .is_none()
        );
        let err = store
            .inspect_entity("赵云", Some("不存在的书"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::NotFound(_)),
            "unknown doc should be NotFound"
        );
    }

    /// Objective: Verify `entity_timeline` orders entries by chapter and
    /// reports the predicate + target for each edge.
    /// Invariants: a 2-edge entity yields 2 entries sorted ascending by chapter.
    #[tokio::test]
    async fn timeline_orders_by_chapter() {
        let store = fresh().await;
        let did = seed_doc(&store, "三国演义").await;
        let lvbu = seed_person(&store, did, "吕布", json!({})).await;
        let dingyuan = seed_person(&store, did, "丁原", json!({})).await;

        // 吕布 kills 丁原 at ch3, serves 丁原 at ch1 — insert out of order.
        for (pred, ch) in [("kills", 3), ("serves", 1)] {
            store
                .create_edge(&KnowledgeEdge {
                    id: 0,
                    source_id: lvbu,
                    target_id: dingyuan,
                    predicate: pred.into(),
                    properties: json!({}),
                    origin: Origin::Observed,
                    confidence: 0.8,
                    valid_from: Some(ch),
                    valid_to: None,
                    created_at: now_ts(),
                })
                .await
                .expect("create edge");
        }
        let tl = store
            .entity_timeline("吕布", Some("三国演义"))
            .await
            .expect("timeline");
        assert_eq!(tl.len(), 2);
        assert_eq!(tl[0].chapter, 1, "serves@ch1 must come first");
        assert_eq!(tl[0].predicate, "serves");
        assert_eq!(tl[1].chapter, 3);
        assert_eq!(tl[1].predicate, "kills");
        assert_eq!(tl[0].target, "丁原");
    }

    /// Objective: Verify `relation_graph` BFS returns the root + 1-hop
    /// neighbors and dedupes edges via petgraph.
    /// Invariants: depth=1 around 刘备 (结义 关羽, 结义 张飞) yields 3 nodes
    /// and 2 edges.
    #[tokio::test]
    async fn relation_graph_bfs_dedupes() {
        let store = fresh().await;
        let did = seed_doc(&store, "三国演义").await;
        let liubei = seed_person(&store, did, "刘备", json!({})).await;
        let guanyu = seed_person(&store, did, "关羽", json!({})).await;
        let zhangfei = seed_person(&store, did, "张飞", json!({})).await;
        for tgt in [guanyu, zhangfei] {
            store
                .create_edge(&KnowledgeEdge {
                    id: 0,
                    source_id: liubei,
                    target_id: tgt,
                    predicate: "结义".into(),
                    properties: json!({}),
                    origin: Origin::Observed,
                    confidence: 1.0,
                    valid_from: Some(1),
                    valid_to: None,
                    created_at: now_ts(),
                })
                .await
                .expect("create edge");
        }
        let g = store
            .relation_graph("刘备", 1, Some("三国演义"))
            .await
            .expect("graph")
            .expect("root found");
        assert_eq!(g.nodes.len(), 3, "root + 2 brothers");
        let names: HashSet<String> = g.nodes.iter().map(|n| n.name.clone()).collect();
        assert!(names.contains("关羽"));
        assert!(names.contains("张飞"));
        assert_eq!(g.edges.len(), 2, "two 结义 edges");
    }

    /// Objective: Verify `relation_graph` returns exactly `depth` hops of
    /// nodes — not `depth+1` (regression for the `0..=depth` off-by-one).
    /// Invariants: a linear chain 刘备→关羽→曹操 with depth=1 yields {刘备, 关羽}
    /// and one edge; node 曹操 (2 hops) must NOT appear.
    #[tokio::test]
    async fn relation_graph_depth_does_not_overreach() {
        let store = fresh().await;
        let did = seed_doc(&store, "三国演义").await;
        let liubei = seed_person(&store, did, "刘备", json!({})).await;
        let guanyu = seed_person(&store, did, "关羽", json!({})).await;
        let caocao = seed_person(&store, did, "曹操", json!({})).await;
        // Chain: 刘备 → 关羽 → 曹操
        for (src, tgt) in [(liubei, guanyu), (guanyu, caocao)] {
            store
                .create_edge(&KnowledgeEdge {
                    id: 0,
                    source_id: src,
                    target_id: tgt,
                    predicate: "结义".into(),
                    properties: json!({}),
                    origin: Origin::Observed,
                    confidence: 1.0,
                    valid_from: Some(1),
                    valid_to: None,
                    created_at: now_ts(),
                })
                .await
                .expect("create edge");
        }
        let g = store
            .relation_graph("刘备", 1, Some("三国演义"))
            .await
            .expect("graph")
            .expect("root found");
        let names: HashSet<String> = g.nodes.iter().map(|n| n.name.clone()).collect();
        assert!(
            names.contains("刘备") && names.contains("关羽"),
            "depth=1 must include root + direct neighbor"
        );
        assert!(
            !names.contains("曹操"),
            "depth=1 must NOT include 2-hop node 曹操 (off-by-one regression)"
        );
        assert_eq!(g.edges.len(), 1, "depth=1 chain yields one edge");
    }

    /// Objective: Verify `relation_graph` preserves parallel edges between
    /// the same pair (regression for DiGraphMap collapsing them to last-wins).
    /// Invariants: two edges 刘备→关羽 ("结义" and "trusts") yield 2 edges with
    /// both predicates present.
    #[tokio::test]
    async fn relation_graph_keeps_parallel_edges() {
        let store = fresh().await;
        let did = seed_doc(&store, "三国演义").await;
        let liubei = seed_person(&store, did, "刘备", json!({})).await;
        let guanyu = seed_person(&store, did, "关羽", json!({})).await;
        for pred in ["结义", "trusts"] {
            store
                .create_edge(&KnowledgeEdge {
                    id: 0,
                    source_id: liubei,
                    target_id: guanyu,
                    predicate: pred.into(),
                    properties: json!({}),
                    origin: Origin::Observed,
                    confidence: 0.9,
                    valid_from: Some(1),
                    valid_to: None,
                    created_at: now_ts(),
                })
                .await
                .expect("create edge");
        }
        let g = store
            .relation_graph("刘备", 1, Some("三国演义"))
            .await
            .expect("graph")
            .expect("root found");
        let preds: HashSet<String> = g.edges.iter().map(|e| e.predicate.clone()).collect();
        assert!(
            preds.contains("结义") && preds.contains("trusts"),
            "parallel edges must both survive, got predicates {preds:?}"
        );
        assert_eq!(g.edges.len(), 2, "two distinct relations → two edges");
    }

    /// Objective: Verify `search_evidence` matches content by substring and
    /// can be scoped to a document.
    /// Invariants: a LIKE query returns matching evidence with chapter + doc.
    #[tokio::test]
    async fn search_evidence_matches_content() {
        let store = fresh().await;
        let did = seed_doc(&store, "三国演义").await;
        let cid = seed_chapter(&store, did, 41).await;
        store
            .create_evidence(&Evidence {
                id: 0,
                doc_id: did,
                chapter_id: cid,
                start_offset: None,
                end_offset: None,
                content: "赵云单骑救阿斗".into(),
                created_at: now_ts(),
            })
            .await
            .expect("create evidence");
        let hits = store
            .search_evidence("阿斗", Some("三国演义"), 10)
            .await
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chapter, 41);
        assert_eq!(hits[0].doc, "三国演义");
        assert_eq!(hits[0].confidence, 1.0);
    }
}
