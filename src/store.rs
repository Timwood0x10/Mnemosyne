use std::sync::Arc;
use std::sync::OnceLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use tokio::sync::Mutex;

use crate::error::{Result, StorageError};
use crate::types::{Experience, ExtractionMethod, MemoryType, Metadata};

static SQLITE_VEC_INIT: OnceLock<()> = OnceLock::new();

fn ensure_vec_loaded() {
    SQLITE_VEC_INIT.get_or_init(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(
            sqlite_vec::sqlite3_vec_init as *const ()
        )));
    });
}

static SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS memories (
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
    expires_at  TEXT NOT NULL DEFAULT '',
    metadata    TEXT NOT NULL DEFAULT '{}',
    vector      TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS idx_memories_tenant ON memories(tenant_id);
CREATE INDEX IF NOT EXISTS idx_memories_type ON memories(memory_type);
CREATE INDEX IF NOT EXISTS idx_memories_tenant_type ON memories(tenant_id, memory_type);
";

static VEC_SCHEMA: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS vec_memories USING vec0(
    id TEXT PRIMARY KEY,
    vector float[?] distance_metric=cosine
);
";

/// Read the embedding dimension baked into an existing `vec_memories`
/// virtual table's DDL (`vector float[N]`), if the table already exists.
///
/// `CREATE VIRTUAL TABLE IF NOT EXISTS` silently keeps a stale schema, so a
/// database created with one dimension would otherwise carry the old
/// dimension into a process opened with a different one — the mismatch then
/// only surfaces at query time (bug-audit store.rs:264-267). Returning the
/// stored dimension lets `init` rebuild the table eagerly instead.
///
/// Returns `Ok(None)` when the table does not exist or its DDL has no
/// `float[N]` marker (both mean "nothing to compare against").
fn stored_vec_dim(conn: &Connection) -> Result<Option<usize>> {
    let sql = match conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='vec_memories'",
        [],
        |r| r.get::<_, String>(0),
    ) {
        Ok(s) => Some(s),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => {
            return Err(StorageError::Sqlite(format!("read vec schema: {e}")).into());
        }
    };
    let Some(sql) = sql else {
        return Ok(None);
    };
    let marker = "float[";
    let Some(start) = sql.find(marker) else {
        return Ok(None);
    };
    let rest = &sql[start + marker.len()..];
    let Some(end) = rest.find(']') else {
        return Ok(None);
    };
    rest[..end]
        .trim()
        .parse::<usize>()
        .map(Some)
        .map_err(|e| StorageError::InvalidData(format!("bad vec dim in DDL: {e}")).into())
}

static FTS_SCHEMA: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
    content, problem, solution,
    tokenize='unicode61'
);
CREATE TRIGGER IF NOT EXISTS memories_fts_insert AFTER INSERT ON memories BEGIN
    INSERT INTO memories_fts(rowid, content, problem, solution)
    VALUES (new.rowid, new.content, new.problem, new.solution);
END;
CREATE TRIGGER IF NOT EXISTS memories_fts_delete AFTER DELETE ON memories BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, content, problem, solution)
    VALUES ('delete', old.rowid, NULL, NULL, NULL);
END;
CREATE TRIGGER IF NOT EXISTS memories_fts_update AFTER UPDATE ON memories BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, content, problem, solution)
    VALUES ('delete', old.rowid, NULL, NULL, NULL);
    INSERT INTO memories_fts(rowid, content, problem, solution)
    VALUES (new.rowid, new.content, new.problem, new.solution);
END;
";

fn memory_type_from_str(s: &str) -> MemoryType {
    s.parse().unwrap_or(MemoryType::Knowledge)
}

fn memory_type_to_str(mt: MemoryType) -> &'static str {
    mt.as_str()
}

fn extraction_method_from_str(s: &str) -> ExtractionMethod {
    match s {
        "cross-turn" => ExtractionMethod::CrossTurn,
        _ => ExtractionMethod::Direct,
    }
}

fn extraction_method_to_str(em: ExtractionMethod) -> &'static str {
    em.as_str()
}

/// Escape a free-text query for safe use inside an FTS5 `MATCH` expression.
///
/// FTS5 raises a syntax error on bare special characters (`"`, `:`, `(`,
/// `*`, …), which would otherwise fail the entire search. We split the
/// query into tokens on FTS5-significant punctuation, wrap each surviving
/// token in double quotes (doubling any embedded quotes), and join them
/// with spaces (implicit AND, matching the prior behaviour). If the query
/// contains no usable tokens, a sentinel that matches nothing is returned so
/// the `MATCH` subquery never raises a syntax error.
fn fts5_query(query: &str) -> String {
    let tokens: Vec<String> = query
        .split(|c: char| {
            c.is_whitespace()
                || c == '"'
                || c == '('
                || c == ')'
                || c == ':'
                || c == ','
                || c == ';'
                || c == '.'
                || c == '*'
                || c == '+'
                || c == '-'
                || c == '^'
                || c == '~'
        })
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    if tokens.is_empty() {
        // Sentinel that matches no row but is always syntactically valid.
        "\"__memory_distill_no_match__\"".to_string()
    } else {
        tokens.join(" ")
    }
}

#[async_trait]
pub trait ExperienceRepository: Send + Sync {
    async fn create(&self, exp: &Experience) -> Result<()>;
    async fn get(&self, id: &str) -> Result<Option<Experience>>;
    async fn update(&self, exp: &Experience) -> Result<()>;
    async fn delete(&self, id: &str) -> Result<()>;
    async fn delete_batch(&self, ids: &[String]) -> Result<()>;
    /// Delete all memories for a tenant whose `expires_at` is in the past.
    /// Returns the number of forgotten memories.
    async fn forget_expired(&self, tenant_id: &str, now: DateTime<Utc>) -> Result<usize>;
    async fn search_by_vector(
        &self,
        query_embedding: &[f32],
        tenant_id: &str,
        limit: usize,
    ) -> Result<Vec<Experience>>;
    async fn get_by_memory_type(
        &self,
        tenant_id: &str,
        memory_type: MemoryType,
    ) -> Result<Vec<Experience>>;
    async fn count_by_memory_type(&self, tenant_id: &str, memory_type: MemoryType) -> Result<i64>;
    async fn count_for_tenant(&self, tenant_id: &str) -> Result<i64>;
    async fn counts_by_type(&self, tenant_id: &str) -> Result<Vec<(MemoryType, i64)>>;
    /// Keyword search: FTS5 when no vector dim, BM25 full-scan otherwise.
    async fn search_by_keyword(
        &self,
        query: &str,
        tenant_id: &str,
        limit: usize,
        memory_type: Option<MemoryType>,
    ) -> Result<Vec<Experience>>;

    /// Load the stored embedding vector for a memory by id.
    ///
    /// Returns an empty vector when the store has no vectors (keyword-only
    /// mode, `dim == 0`) or when the id has no stored vector. This is used by
    /// the conflict-resolution phase to compare a new memory against the real
    /// embedding of an existing one — `search_by_vector` intentionally does
    /// not return the raw vector (only the distance).
    async fn get_vector(&self, id: &str) -> Result<Vec<f32>>;
}

pub struct SQLiteVecStore {
    conn: Arc<Mutex<Connection>>,
    dim: usize,
}

impl std::fmt::Debug for SQLiteVecStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SQLiteVecStore")
            .field("dim", &self.dim)
            .finish_non_exhaustive()
    }
}

impl SQLiteVecStore {
    pub async fn open(path: &str, dim: usize) -> Result<Self> {
        if dim > 0 {
            ensure_vec_loaded();
        }
        let conn =
            Connection::open(path).map_err(|e| StorageError::Schema(format!("open: {e}")))?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            dim,
        };
        store.init().await?;
        Ok(store)
    }

    pub async fn open_in_memory(dim: usize) -> Result<Self> {
        if dim > 0 {
            ensure_vec_loaded();
        }
        let conn = Connection::open_in_memory()
            .map_err(|e| StorageError::Schema(format!("open_in_memory: {e}")))?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            dim,
        };
        store.init().await?;
        Ok(store)
    }

    async fn init(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch(SCHEMA)
            .map_err(|e| StorageError::Schema(format!("init schema: {e}")))?;
        // Migrate older databases that lack the vector column.
        let has_vec_col = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('memories') WHERE name = 'vector'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;
        if !has_vec_col {
            conn.execute(
                "ALTER TABLE memories ADD COLUMN vector TEXT NOT NULL DEFAULT '[]'",
                [],
            )
            .map_err(|e| StorageError::Schema(format!("migrate vector col: {e}")))?;
        }
        // Migrate older databases that lack the expires_at column (added for
        // the TTL forget-expired lifecycle phase). Mirrors the vector column
        // migration above so pre-existing DB files keep opening.
        let has_expires_col = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('memories') WHERE name = 'expires_at'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;
        if !has_expires_col {
            conn.execute(
                "ALTER TABLE memories ADD COLUMN expires_at TEXT NOT NULL DEFAULT ''",
                [],
            )
            .map_err(|e| StorageError::Schema(format!("migrate expires_at col: {e}")))?;
        }
        // Recreate the index in case the column was just added (CREATE INDEX
        // in SCHEMA may have failed on old tables missing the column).
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memories_expires ON memories(expires_at)",
            [],
        );
        if self.dim > 0 {
            // Rebuild the vec table when an existing database was created
            // with a different embedding dimension: `CREATE VIRTUAL TABLE IF
            // NOT EXISTS` silently keeps the stale schema, so the mismatch
            // would otherwise only surface at query time (bug-audit
            // store.rs:264-267). Dropping first forces the table to be
            // recreated with the current dimension.
            if let Some(stored) = stored_vec_dim(&conn)? {
                if stored != self.dim {
                    conn.execute_batch("DROP TABLE IF EXISTS vec_memories;")
                        .map_err(|e| StorageError::Schema(format!("drop stale vec table: {e}")))?;
                }
            }
            let vec_sql = VEC_SCHEMA.replace("?", &self.dim.to_string());
            conn.execute_batch(&vec_sql)
                .map_err(|e| StorageError::Schema(format!("init vec: {e}")))?;
        } else {
            conn.execute_batch(FTS_SCHEMA)
                .map_err(|e| StorageError::Schema(format!("init fts: {e}")))?;
        }
        Ok(())
    }
}

fn row_to_experience(row: &rusqlite::Row) -> rusqlite::Result<Experience> {
    let created_at_str: String = row.get("created_at")?;
    let created_at: DateTime<Utc> = created_at_str.parse().unwrap_or_else(|_| Utc::now());
    let metadata_str: String = row.get("metadata").unwrap_or_default();
    let metadata: Metadata = serde_json::from_str(&metadata_str).unwrap_or_default();

    // distance is only present on vector-search joins; default to 0.
    let distance: f64 = row.get("distance").unwrap_or(0.0);
    Ok(Experience {
        id: row.get("id")?,
        tenant_id: row.get("tenant_id")?,
        user_id: row.get("user_id")?,
        memory_type: memory_type_from_str(row.get::<_, String>("memory_type")?.as_str()),
        problem: row.get("problem")?,
        solution: row.get("solution")?,
        content: row.get("content")?,
        confidence: row.get("confidence")?,
        source: row.get("source")?,
        vector: {
            let s: String = row.get("vector").unwrap_or_default();
            if s.is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&s).unwrap_or_default()
            }
        },
        extraction_method: extraction_method_from_str(
            row.get::<_, String>("extraction_method")?.as_str(),
        ),
        created_at,
        expires_at: {
            let s: String = row.get("expires_at").unwrap_or_default();
            if s.is_empty() {
                None
            } else {
                s.parse::<DateTime<Utc>>().ok()
            }
        },
        metadata,
        distance,
    })
}

#[async_trait]
impl ExperienceRepository for SQLiteVecStore {
    async fn create(&self, exp: &Experience) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let vector_json = serde_json::to_string(&exp.vector).unwrap_or_else(|_| "[]".to_string());
        // Single transaction: `memories` and `vec_memories` must land (or
        // neither). Without it a vec insert failure left the memory row
        // persisted while the caller got an error — a retry then hit the PK
        // conflict on `memories.id`.
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO memories (id, tenant_id, user_id, memory_type, problem, solution, content, confidence, source, extraction_method, created_at, expires_at, metadata, vector)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                exp.id, exp.tenant_id, exp.user_id,
                memory_type_to_str(exp.memory_type),
                exp.problem, exp.solution, exp.content,
                exp.confidence, exp.source,
                extraction_method_to_str(exp.extraction_method),
                exp.created_at.to_rfc3339(),
                exp.expires_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
                serde_json::to_string(&exp.metadata).unwrap_or_default(),
                vector_json,
            ],
        )?;

        if !exp.vector.is_empty() {
            let vec_json = serde_json::to_string(&exp.vector)
                .map_err(|e| StorageError::Schema(format!("serialize vector: {e}")))?;
            tx.execute(
                "INSERT OR REPLACE INTO vec_memories (id, vector) VALUES (?1, ?2)",
                params![exp.id, vec_json],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<Experience>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT * FROM memories WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![id], row_to_experience)?;
        match rows.next() {
            Some(Ok(exp)) => Ok(Some(exp)),
            Some(Err(e)) => Err(StorageError::Sqlite(format!("get row: {e}")).into()),
            None => Ok(None),
        }
    }

    async fn update(&self, exp: &Experience) -> Result<()> {
        let mut conn = self.conn.lock().await;
        let vector_json = serde_json::to_string(&exp.vector).unwrap_or_else(|_| "[]".to_string());
        // Single transaction: the memories UPDATE and the vec_memories
        // upsert must land together, mirroring `create`.
        let tx = conn.transaction()?;
        let affected = tx.execute(
            "UPDATE memories SET tenant_id=?2, user_id=?3, memory_type=?4, problem=?5, solution=?6, content=?7, confidence=?8, source=?9, extraction_method=?10, created_at=?11, metadata=?12, vector=?13, expires_at=?14 WHERE id=?1",
            params![
                exp.id, exp.tenant_id, exp.user_id,
                memory_type_to_str(exp.memory_type),
                exp.problem, exp.solution, exp.content,
                exp.confidence, exp.source,
                extraction_method_to_str(exp.extraction_method),
                exp.created_at.to_rfc3339(),
                serde_json::to_string(&exp.metadata).unwrap_or_default(),
                vector_json,
                exp.expires_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
            ],
        )?;
        if affected == 0 {
            return Err(StorageError::NotFound(exp.id.clone()).into());
        }
        if !exp.vector.is_empty() {
            let vec_json = serde_json::to_string(&exp.vector)
                .map_err(|e| StorageError::Schema(format!("serialize vector: {e}")))?;
            tx.execute(
                "INSERT OR REPLACE INTO vec_memories (id, vector) VALUES (?1, ?2)",
                params![exp.id, vec_json],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        let affected = conn.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
        if affected == 0 {
            return Err(StorageError::NotFound(id.to_string()).into());
        }
        let _ = conn.execute("DELETE FROM vec_memories WHERE id = ?1", params![id]);
        Ok(())
    }

    async fn delete_batch(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().await;
        // Single transaction: either every id is removed from both tables or
        // none is — a mid-batch failure must not leave a half-deleted state
        // while reporting success (the previous per-row `let _ =` swallowed
        // errors and could silently skip rows).
        let tx = conn.transaction()?;
        for id in ids {
            tx.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
            tx.execute("DELETE FROM vec_memories WHERE id = ?1", params![id])?;
        }
        tx.commit()?;
        Ok(())
    }

    async fn forget_expired(&self, tenant_id: &str, now: DateTime<Utc>) -> Result<usize> {
        let now_str = now.to_rfc3339();
        let conn = self.conn.lock().await;
        // Select expired rows, then delete them from both the main table and
        // the vec index. FTS5 cleanup is handled by the DELETE trigger on
        // `memories`. Delete errors propagate so a failed purge never reports
        // a false success count or leaves the indexes inconsistent.
        let ids: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT id FROM memories WHERE tenant_id = ?1 AND expires_at <> '' AND expires_at < ?2",
            )?;
            let rows =
                stmt.query_map(params![tenant_id, now_str], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut count = 0usize;
        for id in &ids {
            let deleted = conn.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
            conn.execute("DELETE FROM vec_memories WHERE id = ?1", params![id])?;
            if deleted > 0 {
                count += 1;
            }
        }
        Ok(count)
    }

    async fn search_by_vector(
        &self,
        query_embedding: &[f32],
        tenant_id: &str,
        limit: usize,
    ) -> Result<Vec<Experience>> {
        if self.dim == 0 {
            return Ok(Vec::new());
        }
        if query_embedding.len() != self.dim {
            return Err(StorageError::DimensionMismatch {
                expected: self.dim,
                actual: query_embedding.len(),
            }
            .into());
        }
        let conn = self.conn.lock().await;
        let vec_json = serde_json::to_string(&query_embedding)
            .map_err(|e| StorageError::Schema(format!("serialize query: {e}")))?;

        let sql = "SELECT m.*, distance FROM memories m
                   JOIN (SELECT id, distance FROM vec_memories
                         WHERE vector MATCH ?1 AND k = ?2) v ON m.id = v.id
                   WHERE m.tenant_id = ?3
                   ORDER BY v.distance ASC
                   LIMIT ?2";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(
            params![vec_json, limit as i64, tenant_id],
            row_to_experience,
        )?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        // results is already ordered by distance ASC by the SQL.
        Ok(results)
    }

    async fn search_by_keyword(
        &self,
        query: &str,
        tenant_id: &str,
        limit: usize,
        memory_type: Option<MemoryType>,
    ) -> Result<Vec<Experience>> {
        if self.dim == 0 {
            // FTS5 path with LIKE fallback for CJK.
            let conn = self.conn.lock().await;
            let like = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
            let has_type_filter = memory_type.is_some();
            let sql = if has_type_filter {
                "SELECT m.* FROM memories m \
                 WHERE (m.rowid IN (SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?1) \
                        OR m.content LIKE ?3 ESCAPE '\\' \
                        OR m.problem LIKE ?3 ESCAPE '\\') \
                   AND m.tenant_id = ?2 AND m.memory_type = ?4 \
                 ORDER BY CASE WHEN m.rowid IN (SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?1) THEN 0 ELSE 1 END \
                 LIMIT ?5"
            } else {
                "SELECT m.* FROM memories m \
                 WHERE (m.rowid IN (SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?1) \
                        OR m.content LIKE ?3 ESCAPE '\\' \
                        OR m.problem LIKE ?3 ESCAPE '\\') \
                   AND m.tenant_id = ?2 \
                 ORDER BY CASE WHEN m.rowid IN (SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?1) THEN 0 ELSE 1 END \
                 LIMIT ?4"
            };
            let mut stmt = conn.prepare(sql)?;
            let rows: Vec<rusqlite::Result<Experience>> = match memory_type {
                Some(mt) => {
                    let r = stmt.query_map(
                        params![
                            fts5_query(query),
                            tenant_id,
                            like,
                            memory_type_to_str(mt),
                            limit as i64
                        ],
                        row_to_experience,
                    )?;
                    r.collect()
                }
                None => {
                    let r = stmt.query_map(
                        params![fts5_query(query), tenant_id, like, limit as i64],
                        row_to_experience,
                    )?;
                    r.collect()
                }
            };
            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            Ok(results)
        } else {
            // vec0 path: full-scan + Rust BM25
            use crate::config::{WEIGHT_IMPORTANCE_ONLY, WEIGHT_KEYWORD_ONLY};
            use crate::retrieval::{bm25_score, tokenize};
            let candidates = if let Some(mt) = memory_type {
                self.get_by_memory_type(tenant_id, mt).await?
            } else {
                let mut all = Vec::new();
                for mt in [
                    MemoryType::Knowledge,
                    MemoryType::Preference,
                    MemoryType::Skill,
                    MemoryType::Experience,
                    MemoryType::Interaction,
                    MemoryType::Profile,
                ] {
                    let exps = self.get_by_memory_type(tenant_id, mt).await?;
                    all.extend(exps);
                }
                all
            };
            let query_terms = tokenize(query);
            let mut scored: Vec<(f64, Experience)> = Vec::new();
            for exp in candidates {
                let kw = bm25_score(&query_terms, &exp.content);
                if kw <= 0.0 {
                    continue;
                }
                scored.push((
                    kw * WEIGHT_KEYWORD_ONLY + exp.confidence * WEIGHT_IMPORTANCE_ONLY,
                    exp,
                ));
            }
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(limit);
            Ok(scored.into_iter().map(|(_, e)| e).collect())
        }
    }

    async fn get_vector(&self, id: &str) -> Result<Vec<f32>> {
        if self.dim == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT vector FROM memories WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![id], |row| {
            let s: String = row.get(0)?;
            Ok(s)
        })?;
        match rows.next() {
            Some(Ok(s)) => {
                if s.is_empty() {
                    Ok(Vec::new())
                } else {
                    serde_json::from_str(&s)
                        .map_err(|e| StorageError::Schema(format!("parse vector: {e}")).into())
                }
            }
            Some(Err(e)) => Err(StorageError::Sqlite(format!("get_vector: {e}")).into()),
            None => Ok(Vec::new()),
        }
    }

    async fn get_by_memory_type(
        &self,
        tenant_id: &str,
        memory_type: MemoryType,
    ) -> Result<Vec<Experience>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT * FROM memories WHERE tenant_id = ?1 AND memory_type = ?2 ORDER BY created_at DESC"
        )?;
        let rows = stmt.query_map(
            params![tenant_id, memory_type_to_str(memory_type)],
            row_to_experience,
        )?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    async fn count_by_memory_type(&self, tenant_id: &str, memory_type: MemoryType) -> Result<i64> {
        let conn = self.conn.lock().await;
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE tenant_id = ?1 AND memory_type = ?2",
            params![tenant_id, memory_type_to_str(memory_type)],
            |row| row.get(0),
        )?)
    }

    async fn count_for_tenant(&self, tenant_id: &str) -> Result<i64> {
        let conn = self.conn.lock().await;
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE tenant_id = ?1",
            params![tenant_id],
            |row| row.get(0),
        )?)
    }

    async fn counts_by_type(&self, tenant_id: &str) -> Result<Vec<(MemoryType, i64)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT memory_type, COUNT(*) as cnt FROM memories WHERE tenant_id = ?1 GROUP BY memory_type ORDER BY cnt DESC"
        )?;
        let rows = stmt.query_map(params![tenant_id], |row| {
            let mt_str: String = row.get("memory_type")?;
            let cnt: i64 = row.get("cnt")?;
            Ok((memory_type_from_str(&mt_str), cnt))
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
