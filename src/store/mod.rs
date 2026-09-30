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
    /// Remove `superseded` and insert `replacements` as ONE unit.
    ///
    /// A conflict resolution is a replacement: the old memory is only gone once
    /// the new one is stored. Running the delete and the inserts as separate
    /// operations meant a failure in between destroyed the old memory without
    /// storing the new one — a lost update that no retry can recover, because
    /// the replacement lives in the caller's memory, not in this store.
    ///
    /// # Errors
    ///
    /// Returns a storage error when any write fails; nothing is applied.
    async fn replace_batch(&self, superseded: &[String], replacements: &[Experience])
    -> Result<()>;
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
        // busy_timeout: several connections share one DB file (vec + fact +
        // knowledge); without it a concurrent writer gets SQLITE_BUSY
        // immediately (0 ms default).
        let _ = conn.execute_batch("PRAGMA busy_timeout = 5000;");
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
        let _ = conn.execute_batch("PRAGMA busy_timeout = 5000;");
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
            // Backfill existing rows so a dimension change (or a DB that
            // already had `memories.vector`) does not silently hide every
            // memory from vector search. Rows whose stored vector length
            // differs from `self.dim` are rejected by sqlite-vec; insert them
            // one by one and skip mismatches so open() still succeeds.
            let rows: Vec<(String, String)> = {
                let mut stmt = conn.prepare(
                    "SELECT id, vector FROM memories WHERE vector IS NOT NULL AND vector != '[]'",
                )?;
                let mapped = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
                mapped.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for (id, vector_json) in rows {
                let parsed: Vec<f32> = match serde_json::from_str(&vector_json) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if parsed.len() != self.dim {
                    tracing::warn!(
                        %id,
                        stored_dim = parsed.len(),
                        expected = self.dim,
                        "vec backfill skipped: dimension mismatch"
                    );
                    continue;
                }
                let _ = conn.execute(
                    "INSERT OR REPLACE INTO vec_memories (id, vector) VALUES (?1, ?2)",
                    params![id, vector_json],
                );
            }
        } else {
            conn.execute_batch(FTS_SCHEMA)
                .map_err(|e| StorageError::Schema(format!("init fts: {e}")))?;
            // Backfill rows that predate the FTS tables/triggers — a database
            // first opened in vector mode (dim > 0 builds no FTS table) or a
            // legacy file. The INSERT trigger only fires for new writes, so
            // without this the old rows stay permanently invisible to MATCH.
            // The `NOT IN` guard makes the statement idempotent across opens.
            conn.execute(
                "INSERT INTO memories_fts(rowid, content, problem, solution)
                 SELECT rowid, content, problem, solution FROM memories
                 WHERE rowid NOT IN (SELECT rowid FROM memories_fts)",
                [],
            )
            .map_err(|e| StorageError::Schema(format!("backfill fts: {e}")))?;
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

mod repository;

#[cfg(test)]
mod tests;
