//! `ExperienceRepository` implementation for the SQLite + sqlite-vec store.

use super::*;
use async_trait::async_trait;

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
