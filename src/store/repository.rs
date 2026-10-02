//! `ExperienceRepository` implementation for the SQLite + sqlite-vec store.

use super::*;
use async_trait::async_trait;

use crate::sql::{like_pattern, sql_limit};

#[async_trait]
impl ExperienceRepository for SQLiteVecStore {
    async fn create(&self, exp: &Experience) -> Result<()> {
        let exp = exp.clone();
        self.with_conn(move |conn| {
            // Single transaction: `memories` and `vec_memories` must land (or
            // neither). Without it a vec insert failure left the memory row
            // persisted while the caller got an error — a retry then hit the PK
            // conflict on `memories.id`.
            let tx = conn.transaction()?;
            insert_experience(&tx, &exp)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn get(&self, id: &str) -> Result<Option<Experience>> {
        let id = id.to_owned();
        self.with_conn(move |conn| {
            // Expiry gate: an expired row must not be readable (it used to be
            // returned forever and even served as a `memory_compile` dedup
            // baseline). `expires_at` is an RFC3339 TEXT column, so a lexical
            // comparison against an RFC3339 `now` is chronologically correct;
            // the boundary matches `Experience::is_expired` (`expires_at <=
            // now`).
            let now = Utc::now();
            let mut stmt = conn.prepare(
                "SELECT * FROM memories WHERE id = ?1 AND (expires_at = '' OR expires_at > ?2)",
            )?;
            let mut rows = stmt.query_map(params![id, now.to_rfc3339()], row_to_experience)?;
            match rows.next() {
                Some(Ok(exp)) if !exp.is_expired(now) => Ok(Some(exp)),
                Some(Ok(_)) => Ok(None),
                Some(Err(e)) => Err(StorageError::Sqlite(format!("get row: {e}")).into()),
                None => Ok(None),
            }
        })
        .await
    }

    async fn update(&self, exp: &Experience) -> Result<()> {
        let exp = exp.clone();
        let dim = self.dim;
        self.with_conn(move |conn| {
            let vector_json =
                serde_json::to_string(&exp.vector).unwrap_or_else(|_| "[]".to_string());
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
            } else if dim > 0 {
                // A cleared vector must also drop the stale index row, else the
                // old embedding keeps matching `search_by_vector` forever
                // (phantom near-neighbor with an empty `vector` field).
                tx.execute("DELETE FROM vec_memories WHERE id = ?1", params![exp.id])?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        let dim = self.dim;
        self.with_conn(move |conn| {
            // Single transaction + dim guard: keyword-only mode (dim == 0) never
            // creates `vec_memories`, so the vec delete must be skipped there —
            // and the two-table delete must land together or not at all.
            let tx = conn.transaction()?;
            if delete_experience(&tx, &id, dim)? == 0 {
                // Dropping the transaction rolls it back.
                return Err(StorageError::NotFound(id.clone()).into());
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn delete_batch(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let ids = ids.to_vec();
        let dim = self.dim;
        self.with_conn(move |conn| {
            // Single transaction: either every id is removed from both tables or
            // none is — a mid-batch failure must not leave a half-deleted state
            // while reporting success (the previous per-row `let _ =` swallowed
            // errors and could silently skip rows).
            //
            // `vec_memories` only exists when dim > 0; the default keyword-only
            // config (dim == 0) created FTS tables instead, so the unconditional
            // vec delete failed with "no such table" and capacity eviction
            // (phase_enforce_capacity) could never run.
            let tx = conn.transaction()?;
            for id in &ids {
                delete_experience(&tx, id, dim)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn replace_batch(
        &self,
        superseded: &[String],
        replacements: &[Experience],
    ) -> Result<()> {
        let superseded = superseded.to_vec();
        let replacements = replacements.to_vec();
        let dim = self.dim;
        self.with_conn(move |conn| {
            // ONE transaction for the whole replacement: the superseded rows
            // only disappear once the replacements are stored. Split in two
            // (the pipeline used to delete in its conflict phase and insert
            // later), a failure in between lost the old memory and stored
            // nothing.
            let tx = conn.transaction()?;
            for id in &superseded {
                delete_experience(&tx, id, dim)?;
            }
            for exp in &replacements {
                insert_experience(&tx, exp)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn forget_expired(&self, tenant_id: &str, now: DateTime<Utc>) -> Result<usize> {
        let tenant_id = tenant_id.to_owned();
        let now_str = now.to_rfc3339();
        let dim = self.dim;
        self.with_conn(move |conn| {
            // One transaction for select+delete: the previous loop ran in
            // autocommit, so a failed vec delete (keyword-only mode, no
            // `vec_memories`) left the memories row already gone and returned
            // Err — a partial purge that never reported a success count.
            let tx = conn.transaction()?;
            let ids: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT id FROM memories WHERE tenant_id = ?1 AND expires_at <> '' AND expires_at <= ?2",
                )?;
                let rows =
                    stmt.query_map(params![tenant_id, now_str], |row| row.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            let mut count = 0usize;
            for id in &ids {
                if delete_experience(&tx, id, dim)? > 0 {
                    count += 1;
                }
            }
            tx.commit()?;
            Ok(count)
        })
        .await
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
        let query_embedding = query_embedding.to_vec();
        let tenant_id = tenant_id.to_owned();
        self.with_conn(move |conn| {
            let vec_json = serde_json::to_string(&query_embedding)
                .map_err(|e| StorageError::Schema(format!("serialize query: {e}")))?;

            // Over-fetch before the tenant filter: kNN runs over the WHOLE
            // table, so a small tenant whose rows rank outside the global top-k
            // would otherwise get zero results even though matching rows exist.
            // Pull up to `limit * 16` (capped) nearest neighbors, then filter by
            // tenant and truncate to `limit`.
            //
            // `LIMIT -1` and `k = -1` mean "no limit" in SQLite, so clamp before
            // the cast; an oversized request must not become an unbounded scan.
            let limit = sql_limit(limit);
            let overfetch = limit.saturating_mul(16).max(limit);
            // Expiry gate (see `get`): filter out rows whose `expires_at` is at
            // or before `now` so vector search never resurfaces expired
            // memories.
            let now = Utc::now();
            let sql = "SELECT m.*, distance FROM memories m
                       JOIN (SELECT id, distance FROM vec_memories
                             WHERE vector MATCH ?1 AND k = ?2) v ON m.id = v.id
                       WHERE m.tenant_id = ?3
                         AND (m.expires_at = '' OR m.expires_at > ?5)
                       ORDER BY v.distance ASC
                       LIMIT ?4";
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map(
                params![vec_json, overfetch, tenant_id, limit, now.to_rfc3339()],
                row_to_experience,
            )?;

            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            // `is_expired` is the authoritative boundary; the SQL predicate is
            // the index-friendly prefilter and the two must agree.
            results.retain(|e| !e.is_expired(now));
            // SQL already ordered by distance ASC and limited to `limit`.
            Ok(results)
        })
        .await
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
            let query = query.to_owned();
            let tenant_id = tenant_id.to_owned();
            return self
                .with_conn(move |conn| {
                    // `LIMIT -1` means "no limit"; clamp before the `i64` cast.
                    let limit = sql_limit(limit);
                    // Escape the backslash FIRST: escaping `%`/`_` before it
                    // would leave a lone `\` in the pattern to combine with the
                    // following escape, so a query ending in `\` silently turned
                    // the trailing `%` wildcard into a literal percent sign.
                    let like = like_pattern(&query);
                    let now = Utc::now();
                    let now_str = now.to_rfc3339();
                    let has_type_filter = memory_type.is_some();
                    let sql = if has_type_filter {
                        "SELECT m.* FROM memories m \
                         WHERE (m.rowid IN (SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?1) \
                                OR m.content LIKE ?3 ESCAPE '\\' \
                                OR m.problem LIKE ?3 ESCAPE '\\' \
                                OR m.solution LIKE ?3 ESCAPE '\\') \
                           AND m.tenant_id = ?2 AND m.memory_type = ?4 \
                           AND (m.expires_at = '' OR m.expires_at > ?6) \
                         ORDER BY CASE WHEN m.rowid IN (SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?1) THEN 0 ELSE 1 END \
                         LIMIT ?5"
                    } else {
                        "SELECT m.* FROM memories m \
                         WHERE (m.rowid IN (SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?1) \
                                OR m.content LIKE ?3 ESCAPE '\\' \
                                OR m.problem LIKE ?3 ESCAPE '\\' \
                                OR m.solution LIKE ?3 ESCAPE '\\') \
                           AND m.tenant_id = ?2 \
                           AND (m.expires_at = '' OR m.expires_at > ?5) \
                         ORDER BY CASE WHEN m.rowid IN (SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?1) THEN 0 ELSE 1 END \
                         LIMIT ?4"
                    };
                    let mut stmt = conn.prepare(sql)?;
                    let rows: Vec<rusqlite::Result<Experience>> = match memory_type {
                        Some(mt) => {
                            let r = stmt.query_map(
                                params![
                                    fts5_query(&query),
                                    tenant_id,
                                    like,
                                    memory_type_to_str(mt),
                                    limit,
                                    now_str
                                ],
                                row_to_experience,
                            )?;
                            r.collect()
                        }
                        None => {
                            let r = stmt.query_map(
                                params![fts5_query(&query), tenant_id, like, limit, now_str],
                                row_to_experience,
                            )?;
                            r.collect()
                        }
                    };
                    let mut results = Vec::new();
                    for row in rows {
                        results.push(row?);
                    }
                    results.retain(|e| !e.is_expired(now));
                    Ok(results)
                })
                .await;
        }

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

    async fn get_vector(&self, id: &str) -> Result<Vec<f32>> {
        if self.dim == 0 {
            return Ok(Vec::new());
        }
        let id = id.to_owned();
        self.with_conn(move |conn| {
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
        })
        .await
    }

    async fn get_by_memory_type(
        &self,
        tenant_id: &str,
        memory_type: MemoryType,
    ) -> Result<Vec<Experience>> {
        let tenant_id = tenant_id.to_owned();
        self.with_conn(move |conn| {
            // Expiry gate (see `get`): expired rows must not be listed by type —
            // otherwise they leak even into keyword mode's BM25 candidate set,
            // which funnels through this method.
            let now = Utc::now();
            let mut stmt = conn.prepare(
                "SELECT * FROM memories WHERE tenant_id = ?1 AND memory_type = ?2 \
                 AND (expires_at = '' OR expires_at > ?3) ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map(
                params![tenant_id, memory_type_to_str(memory_type), now.to_rfc3339()],
                row_to_experience,
            )?;
            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            results.retain(|e| !e.is_expired(now));
            Ok(results)
        })
        .await
    }

    async fn count_by_memory_type(&self, tenant_id: &str, memory_type: MemoryType) -> Result<i64> {
        let tenant_id = tenant_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM memories WHERE tenant_id = ?1 AND memory_type = ?2",
                params![tenant_id, memory_type_to_str(memory_type)],
                |row| row.get(0),
            )?)
        })
        .await
    }

    async fn count_for_tenant(&self, tenant_id: &str) -> Result<i64> {
        let tenant_id = tenant_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM memories WHERE tenant_id = ?1",
                params![tenant_id],
                |row| row.get(0),
            )?)
        })
        .await
    }

    async fn counts_by_type(&self, tenant_id: &str) -> Result<Vec<(MemoryType, i64)>> {
        let tenant_id = tenant_id.to_owned();
        self.with_conn(move |conn| {
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
        })
        .await
    }
}

/// Insert one `Experience` into `memories` — and into `vec_memories` when it
/// carries a vector — on an open transaction.
///
/// Shared by `create` and `replace_batch` so the two write paths cannot drift
/// apart (the vector column and the vec table must agree).
///
/// # Errors
///
/// Returns a storage error when the row or its vector cannot be written.
fn insert_experience(tx: &rusqlite::Transaction<'_>, exp: &Experience) -> Result<()> {
    let vector_json = serde_json::to_string(&exp.vector).unwrap_or_else(|_| "[]".to_string());
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
    Ok(())
}

/// Delete one memory — and its vector when the store keeps one (`dim > 0`) — on
/// an open transaction.
///
/// Returns how many `memories` rows were removed, which is what lets `delete`
/// distinguish "removed" from "was never there".
///
/// # Errors
///
/// Returns a storage error when either delete fails.
fn delete_experience(tx: &rusqlite::Transaction<'_>, id: &str, dim: usize) -> Result<usize> {
    let deleted = tx.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
    if dim > 0 {
        tx.execute("DELETE FROM vec_memories WHERE id = ?1", params![id])?;
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    /// Build a sample experience for the read-path tests.
    fn exp(tenant: &str, mt: MemoryType, content: &str) -> Experience {
        Experience::new(tenant, mt, content, 0.8)
    }

    /// Objective: Verify the clamp is wired into the SQL path: a read called
    /// with `usize::MAX` completes and stays bounded instead of degrading into
    /// an unbounded scan.
    /// Invariants: the call returns `Ok` and every matching row.
    #[tokio::test]
    async fn oversized_limit_stays_bounded() {
        let store = SQLiteVecStore::open_in_memory(0).await.expect("open");
        for i in 0..3 {
            let mut e = exp("t1", MemoryType::Knowledge, &format!("needle {i}"));
            e.id = format!("n{i}");
            store.create(&e).await.expect("create row");
        }
        let results = store
            .search_by_keyword("needle", "t1", usize::MAX, None)
            .await
            .expect("an oversized limit must not error");
        assert_eq!(
            results.len(),
            3,
            "the search must still return every matching row"
        );
    }

    /// Objective: Verify a keyword query containing a literal backslash finds
    /// a stored row containing that backslash, and does NOT fall through to
    /// matching a literal `%` (the pre-fix behaviour for a `\`-terminated
    /// query). FTS cannot match a lone `\`, so this exercises the LIKE branch.
    /// Invariants: the backslash row is returned; the `%`-containing decoy is not.
    #[tokio::test]
    async fn keyword_search_matches_literal_backslash() {
        let store = SQLiteVecStore::open_in_memory(0).await.expect("open");
        let mut bs = exp("t1", MemoryType::Knowledge, "alpha\\");
        bs.id = "bs".to_string();
        store.create(&bs).await.expect("create backslash row");
        let mut pct = exp("t1", MemoryType::Knowledge, "50% off");
        pct.id = "pct".to_string();
        store.create(&pct).await.expect("create percent row");

        let results = store
            .search_by_keyword("\\", "t1", 10, None)
            .await
            .expect("search");
        assert!(
            results.iter().any(|e| e.id == "bs"),
            "a backslash query must match the row containing a literal backslash, got {:?}",
            results.iter().map(|e| &e.id).collect::<Vec<_>>()
        );
        assert!(
            results.iter().all(|e| e.id != "pct"),
            "the literal backslash must not degrade into a literal-% match"
        );
    }

    /// Objective: Verify `%` and `_` stay escaped in the LIKE fallback, i.e.
    /// they match literally instead of acting as wildcards (the pre-fix
    /// unescaped pattern matched unrelated rows).
    /// Invariants: `50%` matches the `50%` row but not the `5000` row; `a_b`
    /// matches the `a_b` row but not the `axb` row.
    #[tokio::test]
    async fn keyword_search_escapes_percent_and_underscore() {
        let store = SQLiteVecStore::open_in_memory(0).await.expect("open");
        for (id, content) in [
            ("pct", "50% off"),
            ("pct2", "5000 items"),
            ("us", "a_b c"),
            ("us2", "axb c"),
        ] {
            let mut e = exp("t1", MemoryType::Knowledge, content);
            e.id = id.to_string();
            store.create(&e).await.expect("create row");
        }

        let pct = store
            .search_by_keyword("50%", "t1", 10, None)
            .await
            .expect("search percent");
        assert!(
            pct.iter().any(|e| e.id == "pct"),
            "the literal `50%` row must match"
        );
        assert!(
            pct.iter().all(|e| e.id != "pct2"),
            "escaped `%` must not act as a wildcard matching `5000`"
        );

        let us = store
            .search_by_keyword("a_b", "t1", 10, None)
            .await
            .expect("search underscore");
        assert!(
            us.iter().any(|e| e.id == "us"),
            "the literal `a_b` row must match"
        );
        assert!(
            us.iter().all(|e| e.id != "us2"),
            "escaped `_` must not act as a wildcard matching `axb`"
        );
    }

    /// Objective: Verify expired rows are excluded from EVERY read path while
    /// non-expired rows still come back (previously none of these reads
    /// filtered `expires_at`, so expired memories leaked forever).
    /// Invariants: the expired row is absent from by-id / by-type / keyword /
    /// vector reads; the live row is present in all of them.
    #[tokio::test]
    async fn expired_rows_are_excluded_from_all_read_paths() {
        let store = SQLiteVecStore::open_in_memory(4).await.expect("open");

        let mut expired = exp("t1", MemoryType::Knowledge, "rust async stale");
        expired.id = "expired".to_string();
        expired.vector = vec![1.0, 0.0, 0.0, 0.0];
        expired.expires_at = Some(Utc::now() - Duration::seconds(60));
        store.create(&expired).await.expect("create expired");

        let mut live = exp("t1", MemoryType::Knowledge, "rust async fresh");
        live.id = "live".to_string();
        live.vector = vec![1.0, 0.0, 0.0, 0.0];
        live.expires_at = Some(Utc::now() + Duration::seconds(3600));
        store.create(&live).await.expect("create live");

        assert!(
            store.get("expired").await.expect("get").is_none(),
            "expired row must not be readable by id"
        );
        assert!(
            store.get("live").await.expect("get").is_some(),
            "live row must be readable by id"
        );

        let by_type = store
            .get_by_memory_type("t1", MemoryType::Knowledge)
            .await
            .expect("by type");
        assert!(
            by_type.iter().all(|e| e.id != "expired"),
            "expired row must not be listed by memory type"
        );
        assert!(
            by_type.iter().any(|e| e.id == "live"),
            "live row must be listed by memory type"
        );

        let kw = store
            .search_by_keyword("rust async", "t1", 10, None)
            .await
            .expect("keyword");
        assert!(
            kw.iter().all(|e| e.id != "expired"),
            "expired row must not surface in keyword search"
        );
        assert!(
            kw.iter().any(|e| e.id == "live"),
            "live row must surface in keyword search"
        );

        let vec = store
            .search_by_vector(&[1.0, 0.0, 0.0, 0.0], "t1", 10)
            .await
            .expect("vector");
        assert!(
            vec.iter().all(|e| e.id != "expired"),
            "expired row must not surface in vector search"
        );
        assert!(
            vec.iter().any(|e| e.id == "live"),
            "live row must surface in vector search"
        );
    }

    /// Objective: Verify the FTS/LIKE keyword branch (dim == 0) also honours
    /// the expiry gate, not just the vector branch.
    /// Invariants: an expired matching row is excluded; a live matching row is
    /// returned.
    #[tokio::test]
    async fn expired_row_excluded_from_fts_keyword_search() {
        let store = SQLiteVecStore::open_in_memory(0).await.expect("open");
        let mut expired = exp("t1", MemoryType::Knowledge, "needle expired");
        expired.id = "expired".to_string();
        expired.expires_at = Some(Utc::now() - Duration::seconds(60));
        store.create(&expired).await.expect("create expired");
        let mut live = exp("t1", MemoryType::Knowledge, "needle live");
        live.id = "live".to_string();
        live.expires_at = Some(Utc::now() + Duration::seconds(3600));
        store.create(&live).await.expect("create live");

        let results = store
            .search_by_keyword("needle", "t1", 10, None)
            .await
            .expect("search");
        assert!(
            results.iter().all(|e| e.id != "expired"),
            "FTS keyword search must exclude the expired row"
        );
        assert!(
            results.iter().any(|e| e.id == "live"),
            "FTS keyword search must keep the live row"
        );
    }

    /// Objective: Verify the expiry boundary is inclusive: a row expiring
    /// exactly at `now` counts as expired, `None` never expires, and a
    /// not-yet-reached expiry is live.
    /// Invariants: `is_expired(now)` is true for `expires_at == now` and for a
    /// past expiry, false for `None` and for a future expiry.
    #[test]
    fn experience_is_expired_boundary() {
        let now = Utc::now();
        let mut e = exp("t1", MemoryType::Knowledge, "x");
        assert!(!e.is_expired(now), "None expires_at must never expire");
        e.expires_at = Some(now);
        assert!(
            e.is_expired(now),
            "expiry exactly at now must count as expired (inclusive `<=`)"
        );
        e.expires_at = Some(now + Duration::seconds(1));
        assert!(!e.is_expired(now), "a future expiry must not be expired");
        e.expires_at = Some(now - Duration::seconds(1));
        assert!(e.is_expired(now), "a past expiry must be expired");
    }

    /// Objective: Verify a row whose expiry has already been reached is
    /// excluded by the read query at the exact boundary instant.
    /// Invariants: a row written with `expires_at = Utc::now()` is not
    /// returned by keyword search; a row expiring far in the future is.
    #[tokio::test]
    async fn row_expiring_at_now_is_excluded() {
        let store = SQLiteVecStore::open_in_memory(0).await.expect("open");
        let mut at_now = exp("t1", MemoryType::Knowledge, "boundarytoken edge");
        at_now.id = "edge".to_string();
        at_now.expires_at = Some(Utc::now());
        store.create(&at_now).await.expect("create edge");

        let mut future = exp("t1", MemoryType::Knowledge, "boundarytoken future");
        future.id = "future".to_string();
        future.expires_at = Some(Utc::now() + Duration::seconds(3600));
        store.create(&future).await.expect("create future");

        let results = store
            .search_by_keyword("boundarytoken", "t1", 10, None)
            .await
            .expect("search");
        assert!(
            results.iter().all(|e| e.id != "edge"),
            "a row reaching its expiry must be excluded at the boundary"
        );
        assert!(
            results.iter().any(|e| e.id == "future"),
            "a row with a future expiry must be returned"
        );
    }

    /// Objective: Verify the FTS backfill makes rows written while the store
    /// was in vector mode (no FTS table/trigger existed) findable after
    /// reopening in keyword mode — otherwise they stay invisible to MATCH.
    /// Invariants: the pre-existing row is returned by keyword search after the
    /// keyword-mode reopen; backfill is idempotent across opens.
    #[tokio::test]
    async fn fts_backfill_makes_rows_from_vector_mode_findable() {
        let dir = tempfile::TempDir::new().expect("temp dir for backfill db");
        let db_path = dir.path().join("backfill.db");

        {
            let store = SQLiteVecStore::open(db_path.to_str().expect("utf8 path"), 4)
                .await
                .expect("open in vector mode");
            let mut e = exp("t1", MemoryType::Knowledge, "uniquebigtoken retrievable");
            e.id = "pre".to_string();
            e.vector = vec![0.1, 0.2, 0.3, 0.4];
            store.create(&e).await.expect("create in vector mode");
        }

        // Reopen in keyword mode: FTS tables are created now and the existing
        // row must be backfilled so MATCH can see it.
        let store = SQLiteVecStore::open(db_path.to_str().expect("utf8 path"), 0)
            .await
            .expect("reopen in keyword mode");
        let results = store
            .search_by_keyword("uniquebigtoken", "t1", 10, None)
            .await
            .expect("search");
        assert!(
            results.iter().any(|e| e.id == "pre"),
            "backfill must make pre-existing rows MATCH-able, got {:?}",
            results.iter().map(|e| &e.id).collect::<Vec<_>>()
        );

        // A second reopen must not duplicate/panic (idempotent backfill).
        drop(store);
        let store = SQLiteVecStore::open(db_path.to_str().expect("utf8 path"), 0)
            .await
            .expect("second keyword reopen");
        let again = store
            .search_by_keyword("uniquebigtoken", "t1", 10, None)
            .await
            .expect("search again");
        assert_eq!(
            again.iter().filter(|e| e.id == "pre").count(),
            1,
            "idempotent backfill must not duplicate the FTS row"
        );
    }

    /// Objective: Verify a keyword that appears only in `solution` is found by
    /// the LIKE fallback (it used to scan only `content`/`problem`). The query
    /// is a substring of a whole FTS token, so MATCH cannot serve it.
    /// Invariants: the row whose `solution` contains the substring is returned.
    #[tokio::test]
    async fn keyword_search_finds_token_only_in_solution() {
        let store = SQLiteVecStore::open_in_memory(0).await.expect("open");
        let mut e = exp("t1", MemoryType::Knowledge, "unrelated body");
        e.id = "sol".to_string();
        e.problem = "unrelated problem".to_string();
        e.solution = "zzqunique solution text".to_string();
        store.create(&e).await.expect("create solution row");

        let results = store
            .search_by_keyword("quniqu", "t1", 10, None)
            .await
            .expect("search");
        assert!(
            results.iter().any(|e| e.id == "sol"),
            "a token present only in solution must be found via the LIKE fallback"
        );
    }

    /// Objective: Verify a delete succeeds in keyword-only mode (dim 0), where
    /// the FTS tables and their AFTER DELETE trigger exist. The trigger used the
    /// external-content `'delete'` command with NULL column values, which FTS5
    /// rejects, so every delete (capacity eviction, TTL forget) failed.
    /// Invariants: `delete_batch` returns `Ok` and the row is gone from both the
    /// table and the FTS index.
    #[tokio::test]
    async fn delete_works_in_keyword_only_mode() {
        let store = SQLiteVecStore::open_in_memory(0).await.expect("open");
        let e = exp("t1", MemoryType::Knowledge, "uniquedeletetoken");
        let id = e.id.clone();
        store.create(&e).await.expect("create row");

        store
            .delete_batch(std::slice::from_ref(&id))
            .await
            .expect("delete in keyword-only mode must succeed");

        assert!(
            store.get(&id).await.expect("get").is_none(),
            "the deleted row must be gone from `memories`"
        );
        let results = store
            .search_by_keyword("uniquedeletetoken", "t1", 10, None)
            .await
            .expect("search after delete");
        assert!(
            results.iter().all(|row| row.id != id),
            "the FTS index must not resurrect the deleted row"
        );
    }
}
