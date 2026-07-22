use std::sync::Arc;
use std::sync::OnceLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use tokio::sync::Mutex;

use crate::error::{Result, StorageError};
use crate::types::{Experience, MemoryType, Metadata, ExtractionMethod};

static SQLITE_VEC_INIT: OnceLock<()> = OnceLock::new();

fn ensure_vec_loaded() {
    SQLITE_VEC_INIT.get_or_init(|| {
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(
                // SAFETY: sqlite-vec's init function matches the expected
                // `sqlite3_auto_extension` callback signature.
                std::mem::transmute::<
                    *const (),
                    unsafe extern "C" fn(
                        *mut rusqlite::ffi::sqlite3,
                        *mut *mut std::os::raw::c_char,
                        *const rusqlite::ffi::sqlite3_api_routines,
                    ) -> i32,
                >(sqlite_vec::sqlite3_vec_init as *const ()),
            ));
        }
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
    metadata    TEXT NOT NULL DEFAULT '{}'
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

#[async_trait]
pub trait ExperienceRepository: Send + Sync {
    async fn create(&self, exp: &Experience) -> Result<()>;
    async fn get(&self, id: &str) -> Result<Option<Experience>>;
    async fn update(&self, exp: &Experience) -> Result<()>;
    async fn delete(&self, id: &str) -> Result<()>;
    async fn delete_batch(&self, ids: &[String]) -> Result<()>;
    async fn search_by_vector(&self, query_embedding: &[f32], tenant_id: &str, limit: usize) -> Result<Vec<Experience>>;
    async fn get_by_memory_type(&self, tenant_id: &str, memory_type: MemoryType) -> Result<Vec<Experience>>;
    async fn count_by_memory_type(&self, tenant_id: &str, memory_type: MemoryType) -> Result<i64>;
    async fn count_for_tenant(&self, tenant_id: &str) -> Result<i64>;
    async fn counts_by_type(&self, tenant_id: &str) -> Result<Vec<(MemoryType, i64)>>;
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
        ensure_vec_loaded();
        if dim == 0 {
            return Err(StorageError::Schema("dimension must be > 0".into()).into());
        }
        let conn = Connection::open(path)
            .map_err(|e| StorageError::Schema(format!("open: {e}")))?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            dim,
        };
        store.init().await?;
        Ok(store)
    }

    pub async fn open_in_memory(dim: usize) -> Result<Self> {
        ensure_vec_loaded();
        if dim == 0 {
            return Err(StorageError::Schema("dimension must be > 0".into()).into());
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
        let vec_sql = VEC_SCHEMA.replace("?", &self.dim.to_string());
        conn.execute_batch(&vec_sql)
            .map_err(|e| StorageError::Schema(format!("init vec: {e}")))?;
        Ok(())
    }
}

fn row_to_experience(row: &rusqlite::Row) -> rusqlite::Result<Experience> {
    let created_at_str: String = row.get("created_at")?;
    let created_at: DateTime<Utc> = created_at_str.parse().unwrap_or_else(|_| Utc::now());
    let metadata_str: String = row.get("metadata").unwrap_or_default();
    let metadata: Metadata = serde_json::from_str(&metadata_str).unwrap_or_default();

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
        vector: Vec::new(),
        extraction_method: extraction_method_from_str(
            row.get::<_, String>("extraction_method")?.as_str(),
        ),
        created_at,
        metadata,
    })
}

#[async_trait]
impl ExperienceRepository for SQLiteVecStore {
    async fn create(&self, exp: &Experience) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO memories (id, tenant_id, user_id, memory_type, problem, solution, content, confidence, source, extraction_method, created_at, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                exp.id, exp.tenant_id, exp.user_id,
                memory_type_to_str(exp.memory_type),
                exp.problem, exp.solution, exp.content,
                exp.confidence, exp.source,
                extraction_method_to_str(exp.extraction_method),
                exp.created_at.to_rfc3339(),
                serde_json::to_string(&exp.metadata).unwrap_or_default(),
            ],
        )?;

        if !exp.vector.is_empty() {
            let vec_json = serde_json::to_string(&exp.vector)
                .map_err(|e| StorageError::Schema(format!("serialize vector: {e}")))?;
            conn.execute(
                "INSERT OR REPLACE INTO vec_memories (id, vector) VALUES (?1, ?2)",
                params![exp.id, vec_json],
            )?;
        }

        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<Experience>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT * FROM memories WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![id], row_to_experience)?;
        match rows.next() {
            Some(Ok(exp)) => Ok(Some(exp)),
            Some(Err(e)) => Err(StorageError::Sqlite(format!("get row: {e}")).into()),
            None => Ok(None),
        }
    }

    async fn update(&self, exp: &Experience) -> Result<()> {
        let conn = self.conn.lock().await;
        let affected = conn.execute(
            "UPDATE memories SET tenant_id=?2, user_id=?3, memory_type=?4, problem=?5, solution=?6, content=?7, confidence=?8, source=?9, extraction_method=?10, created_at=?11, metadata=?12 WHERE id=?1",
            params![
                exp.id, exp.tenant_id, exp.user_id,
                memory_type_to_str(exp.memory_type),
                exp.problem, exp.solution, exp.content,
                exp.confidence, exp.source,
                extraction_method_to_str(exp.extraction_method),
                exp.created_at.to_rfc3339(),
                serde_json::to_string(&exp.metadata).unwrap_or_default(),
            ],
        )?;
        if affected == 0 {
            return Err(StorageError::NotFound(exp.id.clone()).into());
        }
        if !exp.vector.is_empty() {
            let vec_json = serde_json::to_string(&exp.vector)
                .map_err(|e| StorageError::Schema(format!("serialize vector: {e}")))?;
            conn.execute(
                "INSERT OR REPLACE INTO vec_memories (id, vector) VALUES (?1, ?2)",
                params![exp.id, vec_json],
            )?;
        }
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        let affected = conn.execute(
            "DELETE FROM memories WHERE id = ?1",
            params![id],
        )?;
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
        let conn = self.conn.lock().await;
        for id in ids {
            let _ = conn.execute("DELETE FROM memories WHERE id = ?1", params![id]);
            let _ = conn.execute("DELETE FROM vec_memories WHERE id = ?1", params![id]);
        }
        Ok(())
    }

    async fn search_by_vector(&self, query_embedding: &[f32], tenant_id: &str, limit: usize) -> Result<Vec<Experience>> {
        if query_embedding.len() != self.dim {
            return Err(StorageError::DimensionMismatch {
                expected: self.dim,
                actual: query_embedding.len(),
            }.into());
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
        let rows = stmt.query_map(params![vec_json, limit as i64, tenant_id], row_to_experience)?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    async fn get_by_memory_type(&self, tenant_id: &str, memory_type: MemoryType) -> Result<Vec<Experience>> {
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
    async fn open_with_zero_dim_fails() {
        let err = SQLiteVecStore::open_in_memory(0).await.unwrap_err();
        assert!(err.to_string().contains("dimension"), "zero dim should fail");
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

        let results = store.search_by_vector(&[1.0_f32, 0.0, 0.0, 0.0], "t1", 5).await.expect("search");
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

        let r1 = store.search_by_vector(&[1.0_f32, 0.0, 0.0, 0.0], "t1", 5).await.expect("search");
        assert!(r1.iter().all(|e| e.tenant_id == "t1"), "only t1 results");
    }

    #[tokio::test]
    async fn search_rejects_dimension_mismatch() {
        let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
        let err = store.search_by_vector(&[1.0_f32, 0.0, 0.0], "t1", 5).await.unwrap_err();
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
        store.delete_batch(&[]).await.expect("empty batch should not error");
    }

    #[tokio::test]
    async fn get_by_memory_type_filters() {
        let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
        store.create(&sample_exp("t1", MemoryType::Knowledge, "k")).await.expect("create");
        store.create(&sample_exp("t1", MemoryType::Preference, "p")).await.expect("create");
        let k = store.get_by_memory_type("t1", MemoryType::Knowledge).await.expect("get");
        let p = store.get_by_memory_type("t1", MemoryType::Preference).await.expect("get");
        assert_eq!(k.len(), 1, "one knowledge");
        assert_eq!(p.len(), 1, "one preference");
    }

    #[tokio::test]
    async fn create_and_search_round_trip() {
        let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
        let mut exp = sample_exp("t1", MemoryType::Knowledge, "rust");
        exp.vector = vec![1.0_f32, 0.0, 0.0, 0.0];
        store.create(&exp).await.expect("create");
        let results = store.search_by_vector(&[1.0_f32, 0.0, 0.0, 0.0], "t1", 10).await.expect("search");
        assert!(!results.is_empty(), "should find created memory");
    }
}
