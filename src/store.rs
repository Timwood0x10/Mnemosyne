use std::sync::Mutex;

use async_trait::async_trait;

use crate::error::{Error, Result, StorageError};
use crate::types::{Experience, ExtractionMethod, MemoryType, Metadata};

/// Register sqlite-vec extension globally (runs once per process lifetime).
fn ensure_vec_extension_loaded() -> Result<()> {
    static INIT: std::sync::Once = std::sync::Once::new();
    let mut err = None;
    INIT.call_once(|| {
        unsafe {
            use rusqlite::auto_extension::register_auto_extension;
            let func: unsafe extern "C" fn() = sqlite_vec::sqlite3_vec_init;
            let raw = std::mem::transmute(func);
            if let Err(e) = register_auto_extension(raw) {
                err = Some(e);
            }
        }
    });
    match err {
        Some(e) => Err(Error::from(e)),
        None => Ok(()),
    }
}

/// Async storage contract for distilled experiences.
#[async_trait]
pub trait ExperienceRepository: Send + Sync {
    async fn create(&self, exp: &Experience) -> Result<()>;

    async fn update(&self, exp: &Experience) -> Result<()>;

    async fn delete(&self, id: &str) -> Result<()>;

    async fn delete_batch(&self, ids: &[String]) -> Result<()>;

    async fn search_by_vector(
        &self,
        vector: &[f32],
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
}

/// SQLite + sqlite-vec backed repository.
pub struct SQLiteVecStore {
    conn: Mutex<rusqlite::Connection>,
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
    pub async fn open(db_path: &str, dim: usize) -> Result<Self> {
        if dim == 0 {
            return Err(StorageError::Schema("dimension must be > 0".into()).into());
        }
        ensure_vec_extension_loaded()?;
        let conn = rusqlite::Connection::open(db_path)
            .map_err(|e| StorageError::Sqlite(format!("open: {e}")))?;
        let store = Self {
            conn: Mutex::new(conn),
            dim,
        };
        store.ensure_tables()?;
        Ok(store)
    }

    pub async fn open_in_memory(dim: usize) -> Result<Self> {
        Self::open(":memory:", dim).await
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    fn ensure_tables(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let dim = self.dim;
        conn.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS experiences (
                id TEXT PRIMARY KEY NOT NULL,
                tenant_id TEXT NOT NULL,
                user_id TEXT NOT NULL DEFAULT '',
                memory_type TEXT NOT NULL,
                problem TEXT NOT NULL DEFAULT '',
                solution TEXT NOT NULL DEFAULT '',
                content TEXT NOT NULL,
                confidence REAL NOT NULL,
                source TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                metadata TEXT NOT NULL DEFAULT '{{}}',
                extraction_method TEXT NOT NULL DEFAULT 'direct',
                vector BLOB NOT NULL DEFAULT x''
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS vec_experiences USING vec0(
                id TEXT PRIMARY KEY,
                vector FLOAT32[{dim}] distance_metric=cosine
            );
            CREATE INDEX IF NOT EXISTS idx_exp_tenant_type
                ON experiences(tenant_id, memory_type);
            CREATE INDEX IF NOT EXISTS idx_exp_tenant
                ON experiences(tenant_id);"
        ))
        .map_err(|e| StorageError::Schema(format!("create tables: {e}")))?;
        Ok(())
    }

    fn insert_vec(&self, conn: &rusqlite::Connection, id: &str, vector: &[f32]) -> Result<()> {
        let json = serde_json::to_string(vector)
            .map_err(|e| StorageError::Sqlite(format!("serialize vector: {e}")))?;
        conn.execute(
            "INSERT INTO vec_experiences(id, vector) VALUES (?1, ?2)",
            rusqlite::params![id, json],
        )
        .map_err(|e| StorageError::Sqlite(format!("insert vec: {e}")))?;
        Ok(())
    }

    fn delete_vec(&self, conn: &rusqlite::Connection, id: &str) -> Result<()> {
        conn.execute(
            "DELETE FROM vec_experiences WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(|e| StorageError::Sqlite(format!("delete vec: {e}")))?;
        Ok(())
    }

    fn experience_to_params(&self, exp: &Experience) -> Result<Vec<Box<dyn rusqlite::types::ToSql>>> {
        let metadata_json = serde_json::to_string(&exp.metadata)
            .unwrap_or_else(|_| "{}".to_string());
        let vector_blob: Vec<u8> = exp
            .vector
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        Ok(vec![
            Box::new(exp.id.clone()) as Box<dyn rusqlite::types::ToSql>,
            Box::new(exp.tenant_id.clone()),
            Box::new(exp.user_id.clone()),
            Box::new(exp.memory_type.as_str().to_string()),
            Box::new(exp.problem.clone()),
            Box::new(exp.solution.clone()),
            Box::new(exp.content.clone()),
            Box::new(exp.confidence),
            Box::new(exp.source.clone()),
            Box::new(exp.created_at.to_rfc3339()),
            Box::new(metadata_json),
            Box::new(exp.extraction_method.as_str().to_string()),
            Box::new(vector_blob),
        ])
    }

    fn row_to_experience(row: &rusqlite::Row) -> rusqlite::Result<Experience> {
        let id: String = row.get("id")?;
        let tenant_id: String = row.get("tenant_id")?;
        let user_id: String = row.get("user_id")?;
        let memory_type_str: String = row.get("memory_type")?;
        let memory_type = memory_type_str.parse::<MemoryType>().map_err(|e| {
            rusqlite::Error::InvalidColumnName(format!("memory_type: {e}"))
        })?;
        let problem: String = row.get("problem")?;
        let solution: String = row.get("solution")?;
        let content: String = row.get("content")?;
        let confidence: f64 = row.get("confidence")?;
        let source: String = row.get("source")?;
        let created_at_str: String = row.get("created_at")?;
        let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now());
        let metadata_str: String = row.get("metadata")?;
        let metadata: Metadata = serde_json::from_str(&metadata_str).unwrap_or_default();
        let method_str: String = row.get("extraction_method")?;
        let extraction_method = match method_str.as_str() {
            "cross-turn" => ExtractionMethod::CrossTurn,
            _ => ExtractionMethod::Direct,
        };
        let vector_blob: Vec<u8> = row.get::<_, Vec<u8>>("vector").unwrap_or_default();
        let vector: Vec<f32> = vector_blob
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        Ok(Experience {
            id,
            tenant_id,
            user_id,
            memory_type,
            problem,
            solution,
            content,
            confidence,
            source,
            vector,
            extraction_method,
            created_at,
            metadata,
        })
    }
}

#[async_trait]
impl ExperienceRepository for SQLiteVecStore {
    async fn create(&self, exp: &Experience) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let params = self.experience_to_params(exp)?;
        let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        conn.execute(
            "INSERT INTO experiences(
                id, tenant_id, user_id, memory_type, problem, solution,
                content, confidence, source, created_at, metadata,
                extraction_method, vector
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            refs.as_slice(),
        )
        .map_err(|e| StorageError::Sqlite(format!("insert: {e}")))?;
        if !exp.vector.is_empty() {
            self.insert_vec(&conn, &exp.id, &exp.vector)?;
        }
        Ok(())
    }

    async fn update(&self, exp: &Experience) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let params = self.experience_to_params(exp)?;
        let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let rows = conn
            .execute(
                "UPDATE experiences SET
                    tenant_id = ?2, user_id = ?3, memory_type = ?4,
                    problem = ?5, solution = ?6, content = ?7,
                    confidence = ?8, source = ?9, created_at = ?10,
                    metadata = ?11, extraction_method = ?12, vector = ?13
                WHERE id = ?1",
                refs.as_slice(),
            )
            .map_err(|e| StorageError::Sqlite(format!("update: {e}")))?;
        if rows == 0 {
            return Err(StorageError::NotFound(exp.id.clone()).into());
        }
        self.delete_vec(&conn, &exp.id)?;
        if !exp.vector.is_empty() {
            self.insert_vec(&conn, &exp.id, &exp.vector)?;
        }
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let rows = conn
            .execute("DELETE FROM experiences WHERE id = ?1", rusqlite::params![id])
            .map_err(|e| StorageError::Sqlite(format!("delete: {e}")))?;
        if rows == 0 {
            return Err(StorageError::NotFound(id.to_string()).into());
        }
        self.delete_vec(&conn, id)?;
        Ok(())
    }

    async fn delete_batch(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        for id in ids {
            conn.execute(
                "DELETE FROM experiences WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(|e| StorageError::Sqlite(format!("delete_batch: {e}")))?;
            self.delete_vec(&conn, id)?;
        }
        Ok(())
    }

    async fn search_by_vector(
        &self,
        vector: &[f32],
        tenant_id: &str,
        limit: usize,
    ) -> Result<Vec<Experience>> {
        if vector.len() != self.dim {
            return Err(StorageError::DimensionMismatch {
                expected: self.dim,
                actual: vector.len(),
            }
            .into());
        }
        let query_json = serde_json::to_string(vector)
            .map_err(|e| StorageError::Sqlite(format!("serialize query vector: {e}")))?;
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT e.*
                 FROM (SELECT id FROM vec_experiences WHERE vector MATCH ?1 LIMIT ?3) v
                 JOIN experiences e ON e.id = v.id
                 WHERE e.tenant_id = ?2",
            )
            .map_err(|e| StorageError::Sqlite(format!("prepare search: {e}")))?;
        let results = stmt
            .query_map(
                rusqlite::params![query_json, tenant_id, limit as i64],
                Self::row_to_experience,
            )
            .map_err(|e| StorageError::Sqlite(format!("query search: {e}")))?;
        let mut out = Vec::with_capacity(limit);
        for row in results {
            out.push(row.map_err(|e| StorageError::Sqlite(format!("row: {e}")))?);
        }
        Ok(out)
    }

    async fn get_by_memory_type(
        &self,
        tenant_id: &str,
        memory_type: MemoryType,
    ) -> Result<Vec<Experience>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT * FROM experiences
                 WHERE tenant_id = ?1 AND memory_type = ?2
                 ORDER BY created_at DESC",
            )
            .map_err(|e| StorageError::Sqlite(format!("prepare get_by_type: {e}")))?;
        let results = stmt
            .query_map(
                rusqlite::params![tenant_id, memory_type.as_str()],
                Self::row_to_experience,
            )
            .map_err(|e| StorageError::Sqlite(format!("query get_by_type: {e}")))?;
        let mut out = Vec::new();
        for row in results {
            out.push(row.map_err(|e| StorageError::Sqlite(format!("row: {e}")))?);
        }
        Ok(out)
    }

    async fn count_by_memory_type(&self, tenant_id: &str, memory_type: MemoryType) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM experiences WHERE tenant_id = ?1 AND memory_type = ?2",
                rusqlite::params![tenant_id, memory_type.as_str()],
                |row| row.get(0),
            )
            .map_err(|e| StorageError::Sqlite(format!("count_by_type: {e}")))?;
        Ok(count)
    }

    async fn count_for_tenant(&self, tenant_id: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM experiences WHERE tenant_id = ?1",
                rusqlite::params![tenant_id],
                |row| row.get(0),
            )
            .map_err(|e| StorageError::Sqlite(format!("count_tenant: {e}")))?;
        Ok(count)
    }

    async fn counts_by_type(&self, tenant_id: &str) -> Result<Vec<(MemoryType, i64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT memory_type, COUNT(*) as cnt FROM experiences
                 WHERE tenant_id = ?1
                 GROUP BY memory_type
                 ORDER BY memory_type",
            )
            .map_err(|e| StorageError::Sqlite(format!("prepare counts_by_type: {e}")))?;
        let results = stmt
            .query_map(rusqlite::params![tenant_id], |row| {
                let mt_str: String = row.get("memory_type")?;
                let count: i64 = row.get("cnt")?;
                Ok((mt_str, count))
            })
            .map_err(|e| StorageError::Sqlite(format!("query counts_by_type: {e}")))?;
        let mut out = Vec::new();
        for row in results {
            let (mt_str, count) = row.map_err(|e| StorageError::Sqlite(format!("row: {e}")))?;
            if let Ok(mt) = mt_str.parse::<MemoryType>() {
                out.push((mt, count));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_in_memory_succeeds() {
        let store = SQLiteVecStore::open_in_memory(8).await.expect("open");
        assert_eq!(store.dim(), 8, "dim getter returns configured value");
    }

    #[tokio::test]
    async fn open_with_zero_dim_fails() {
        let err = SQLiteVecStore::open_in_memory(0).await.unwrap_err();
        assert!(
            matches!(err, Error::Storage(StorageError::Schema(_))),
            "expected Schema error, got {err:?}"
        );
    }

    fn make_exp(tenant: &str, id: &str, content: &str, dim: usize) -> Experience {
        let mut exp = Experience::new(tenant, MemoryType::Knowledge, content, 0.7);
        exp.id = id.to_string();
        exp.vector = vec![1.0_f32; dim];
        exp
    }

    #[tokio::test]
    async fn create_and_search_round_trip() {
        let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
        let exp = make_exp("t1", "id-1", "How do I read a file in Rust?", 4);
        store.create(&exp).await.expect("create");
        let found = store
            .search_by_vector(&exp.vector, "t1", 5)
            .await
            .expect("search");
        assert_eq!(found.len(), 1, "search returns 1 result");
        assert_eq!(found[0].id, "id-1", "returned id matches");
        assert_eq!(found[0].content, "How do I read a file in Rust?");
    }

    #[tokio::test]
    async fn search_is_tenant_scoped() {
        let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
        let exp = make_exp("t1", "id-1", "tenant 1 content", 4);
        store.create(&exp).await.expect("create");
        let found = store
            .search_by_vector(&exp.vector, "t2", 5)
            .await
            .expect("search");
        assert!(found.is_empty(), "tenant 2 search returns nothing");
    }

    #[tokio::test]
    async fn search_rejects_dimension_mismatch() {
        let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
        let bad_vec = vec![1.0_f32; 8];
        let err = store.search_by_vector(&bad_vec, "t1", 5).await.unwrap_err();
        assert!(
            matches!(err, Error::Storage(StorageError::DimensionMismatch { .. })),
            "expected DimensionMismatch, got {err:?}"
        );
    }

    #[tokio::test]
    async fn count_by_memory_type() {
        let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
        for i in 0..3 {
            let exp = make_exp("t1", &format!("k-{i}"), "k", 4);
            store.create(&exp).await.expect("create");
        }
        for i in 0..2 {
            let mut exp = Experience::new("t1", MemoryType::Preference, "p", 0.5);
            exp.id = format!("p-{i}");
            exp.vector = vec![1.0_f32; 4];
            store.create(&exp).await.expect("create");
        }
        let k = store
            .count_by_memory_type("t1", MemoryType::Knowledge)
            .await
            .expect("count");
        let p = store
            .count_by_memory_type("t1", MemoryType::Preference)
            .await
            .expect("count");
        assert_eq!(k, 3, "3 Knowledge records");
        assert_eq!(p, 2, "2 Preference records");
    }

    #[tokio::test]
    async fn delete_removes_record() {
        let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
        let exp = make_exp("t1", "id-1", "x", 4);
        store.create(&exp).await.expect("create");
        store.delete("id-1").await.expect("delete");
        let found = store
            .search_by_vector(&exp.vector, "t1", 5)
            .await
            .expect("search");
        assert!(found.is_empty(), "no results after delete");
    }

    #[tokio::test]
    async fn delete_missing_returns_not_found() {
        let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
        let err = store.delete("nonexistent").await.unwrap_err();
        assert!(
            matches!(err, Error::Storage(StorageError::NotFound(_))),
            "expected NotFound, got {err:?}"
        );
    }

    #[tokio::test]
    async fn update_modifies_record() {
        let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
        let mut exp = make_exp("t1", "id-1", "original", 4);
        store.create(&exp).await.expect("create");
        exp.content = "updated".to_string();
        store.update(&exp).await.expect("update");
        let found = store
            .search_by_vector(&exp.vector, "t1", 5)
            .await
            .expect("search");
        assert_eq!(found[0].content, "updated", "content updated");
    }

    #[tokio::test]
    async fn delete_batch_removes_multiple() {
        let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
        let ids = ["id-1", "id-2", "id-3"];
        for id in &ids {
            let exp = make_exp("t1", id, "x", 4);
            store.create(&exp).await.expect("create");
        }
        let id_strings: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        store
            .delete_batch(&id_strings)
            .await
            .expect("delete_batch");
        let v = vec![1.0_f32; 4];
        let found = store
            .search_by_vector(&v, "t1", 10)
            .await
            .expect("search");
        assert!(found.is_empty(), "all records deleted");
    }

    #[tokio::test]
    async fn delete_batch_empty_noop() {
        let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
        store.delete_batch(&[]).await.expect("no-op");
    }

    #[tokio::test]
    async fn counts_by_type_returns_all() {
        let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
        for id in ["k1", "k2"] {
            let exp = make_exp("t1", id, "k", 4);
            store.create(&exp).await.expect("create");
        }
        let mut p_exp = Experience::new("t1", MemoryType::Preference, "p", 0.5);
        p_exp.id = "p1".to_string();
        p_exp.vector = vec![1.0_f32; 4];
        store.create(&p_exp).await.expect("create");
        let counts = store.counts_by_type("t1").await.expect("counts");
        let k_count = counts
            .iter()
            .find(|(mt, _)| *mt == MemoryType::Knowledge)
            .map(|(_, c)| *c)
            .unwrap_or(0);
        let p_count = counts
            .iter()
            .find(|(mt, _)| *mt == MemoryType::Preference)
            .map(|(_, c)| *c)
            .unwrap_or(0);
        assert_eq!(k_count, 2, "2 Knowledge");
        assert_eq!(p_count, 1, "1 Preference");
    }

    #[tokio::test]
    async fn get_by_memory_type_filters() {
        let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
        let k_exp = make_exp("t1", "k1", "k", 4);
        store.create(&k_exp).await.expect("create");
        let mut p_exp = Experience::new("t1", MemoryType::Preference, "p", 0.5);
        p_exp.id = "p1".to_string();
        p_exp.vector = vec![1.0_f32; 4];
        store.create(&p_exp).await.expect("create");
        let k_results = store
            .get_by_memory_type("t1", MemoryType::Knowledge)
            .await
            .expect("get");
        assert_eq!(k_results.len(), 1, "1 Knowledge result");
        assert_eq!(k_results[0].id, "k1");
    }

    #[tokio::test]
    async fn count_for_tenant_sums_all() {
        let store = SQLiteVecStore::open_in_memory(4).await.expect("open");
        for id in ["k1", "k2"] {
            let exp = make_exp("t1", id, "k", 4);
            store.create(&exp).await.expect("create");
        }
        let mut p_exp = Experience::new("t1", MemoryType::Preference, "p", 0.5);
        p_exp.id = "p1".to_string();
        p_exp.vector = vec![1.0_f32; 4];
        store.create(&p_exp).await.expect("create");
        let count = store.count_for_tenant("t1").await.expect("count");
        assert_eq!(count, 3, "3 total records for t1");
    }
}
