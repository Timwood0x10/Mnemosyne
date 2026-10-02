//! `KnowledgeStore` implementation for [`SQLiteKnowledgeStore`].
//!
//! Split out of `mod.rs` so that file stays under the one-file-per-1000-lines
//! rule (`plan/rules/rules.md` §1). Every method here either delegates to an
//! inherent `*_row` method (`world_io.rs`, `queries.rs`) or sends a small SQL
//! statement to the blocking pool through [`SQLiteKnowledgeStore::with_conn`]
//! (batch G — `rusqlite` is synchronous, so running it on a tokio worker stalls
//! unrelated connections; audit 09-26/H7). The trait itself is declared in
//! `trait_def.rs`; the helpers this file uses (`Connection`, `Error`, `Result`,
//! the row mappers) come from the parent module's imports via `use super::*`.

use super::*;

#[async_trait]
impl KnowledgeStore for SQLiteKnowledgeStore {
    async fn create_document(&self, d: &Document) -> Result<i64> {
        // Owned copy: the closure handed to the blocking pool must be `'static`.
        let d = d.clone();
        // Idempotent by identity (title, source), which the database enforces:
        // every caller does "find_document → None → create_document" under two
        // separate lock acquisitions, so two of them can both decide to insert.
        // Without the upsert one work became two document rows, with its
        // objects, edges and evidence split between them.
        //
        // The conflict branch does not shrink what is already recorded: a
        // caller that supplies no author must not erase the author stored
        // earlier. `RETURNING id`, because `last_insert_rowid()` is NOT updated
        // when the insert takes the `DO UPDATE` branch.
        self.with_conn(move |conn| {
            let id: i64 = conn.query_row(
                "INSERT INTO documents (title, author, doc_type, source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(title, source) DO UPDATE SET
                 author = COALESCE(excluded.author, documents.author),
                 doc_type = COALESCE(excluded.doc_type, documents.doc_type)
             RETURNING id",
                params![d.title, d.author, d.doc_type, d.source, d.created_at],
                |row| row.get(0),
            )?;
            Ok(id)
        })
        .await
    }

    async fn find_document_by_title(&self, title: &str) -> Result<Option<Document>> {
        let title = title.to_string();
        // ORDER BY id: same-titled rows (different sources) make the row
        // order significant — without it SQLite makes no order guarantee
        // and the documented "first created row" contract would be luck.
        self.with_conn(move |conn| {
            let mut stmt =
                conn.prepare("SELECT * FROM documents WHERE title = ?1 ORDER BY id ASC")?;
            let mut rows = stmt.query_map(params![title], row_to_document)?;
            match rows.next() {
                Some(Ok(d)) => Ok(Some(d)),
                Some(Err(e)) => Err(StorageError::Sqlite(format!("find_document: {e}")).into()),
                None => Ok(None),
            }
        })
        .await
    }

    async fn find_document(&self, title: &str, source: &str) -> Result<Option<Document>> {
        let title = title.to_string();
        let source = source.to_string();
        // ORDER BY id, for the same reason as `find_document_by_title`: when
        // several rows share an identity the row returned must be the first one
        // created, not whichever SQLite reaches first.
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT * FROM documents WHERE title = ?1 AND source = ?2 ORDER BY id ASC",
            )?;
            let mut rows = stmt.query_map(params![title, source], row_to_document)?;
            match rows.next() {
                Some(Ok(d)) => Ok(Some(d)),
                Some(Err(e)) => Err(StorageError::Sqlite(format!("find_document: {e}")).into()),
                None => Ok(None),
            }
        })
        .await
    }

    async fn create_chapter(&self, c: &Chapter) -> Result<i64> {
        let c = c.clone();
        self.with_conn(move |conn| {
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
        })
        .await
    }

    async fn get_chapter_by_no(&self, doc_id: i64, chapter_no: i32) -> Result<Option<Chapter>> {
        self.with_conn(move |conn| {
            let mut stmt =
                conn.prepare("SELECT * FROM chapters WHERE doc_id = ?1 AND chapter_no = ?2")?;
            let mut rows = stmt.query_map(params![doc_id, chapter_no], row_to_chapter)?;
            match rows.next() {
                Some(Ok(c)) => Ok(Some(c)),
                Some(Err(e)) => Err(StorageError::Sqlite(format!("get_chapter: {e}")).into()),
                None => Ok(None),
            }
        })
        .await
    }

    async fn create_object(&self, o: &KnowledgeObject) -> Result<i64> {
        let o = o.clone();
        self.with_conn(move |conn| {
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
        })
        .await
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
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare("SELECT * FROM knowledge_objects WHERE id = ?1")?;
            let mut rows = stmt.query_map(params![id], row_to_object)?;
            match rows.next() {
                Some(Ok(o)) => Ok(Some(o)),
                Some(Err(e)) => Err(StorageError::Sqlite(format!("get_object: {e}")).into()),
                None => Ok(None),
            }
        })
        .await
    }

    async fn get_objects_bulk(&self, ids: &[i64]) -> Result<Vec<KnowledgeObject>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // One IN-clause query replaces N per-id lookups (inspect_entity /
        // entity_timeline previously did one `get_object` per event/neighbor).
        let ids = ids.to_vec();
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!("SELECT * FROM knowledge_objects WHERE id IN ({placeholders})");
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), row_to_object)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .await
    }

    async fn find_object_by_name(
        &self,
        name: &str,
        doc_id: Option<i64>,
    ) -> Result<Option<KnowledgeObject>> {
        let name = name.to_string();
        // The boxed parameter list must be `Send` to cross into the blocking
        // pool, hence the `+ Send` on the trait object.
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::types::ToSql + Send>>) = match doc_id
        {
            Some(d) => (
                "SELECT * FROM knowledge_objects WHERE name = ?1 AND doc_id = ?2 \
             ORDER BY confidence DESC LIMIT 1"
                    .to_string(),
                vec![
                    Box::new(name) as Box<dyn rusqlite::types::ToSql + Send>,
                    Box::new(d),
                ],
            ),
            None => (
                "SELECT * FROM knowledge_objects WHERE name = ?1 \
             ORDER BY confidence DESC LIMIT 1"
                    .to_string(),
                vec![Box::new(name) as Box<dyn rusqlite::types::ToSql + Send>],
            ),
        };
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = stmt.query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|p| p.as_ref())),
                row_to_object,
            )?;
            match rows.next() {
                Some(Ok(o)) => Ok(Some(o)),
                Some(Err(e)) => {
                    Err(StorageError::Sqlite(format!("find_object_by_name: {e}")).into())
                }
                None => Ok(None),
            }
        })
        .await
    }

    async fn find_object_by_alias(
        &self,
        name: &str,
        doc_id: Option<i64>,
    ) -> Result<Option<KnowledgeObject>> {
        self.find_object_by_alias_query(name, doc_id).await
    }

    async fn create_edge(&self, e: &KnowledgeEdge) -> Result<i64> {
        let e = e.clone();
        self.with_conn(move |conn| {
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
        })
        .await
    }

    async fn get_edges_touching(&self, object_id: i64) -> Result<Vec<KnowledgeEdge>> {
        self.with_conn(move |conn| {
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
        })
        .await
    }

    async fn update_edge_target(&self, edge_id: i64, new_target_id: i64) -> Result<usize> {
        self.with_conn(move |conn| {
            let n = conn.execute(
                "UPDATE knowledge_edges SET target_id = ?1 WHERE id = ?2",
                params![new_target_id, edge_id],
            )?;
            Ok(n)
        })
        .await
    }

    async fn create_evidence(&self, e: &Evidence) -> Result<i64> {
        let e = e.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO evidence (doc_id, chapter_id, start_offset, end_offset, content, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![e.doc_id, e.chapter_id, e.start_offset, e.end_offset, e.content, e.created_at],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await
    }

    async fn ensure_evidence(
        &self,
        doc_id: i64,
        chapter_id: i64,
        start_offset: Option<i64>,
        end_offset: Option<i64>,
        content: &str,
    ) -> Result<(i64, bool)> {
        self.ensure_evidence_row(doc_id, chapter_id, start_offset, end_offset, content)
            .await
    }

    async fn link_evidence(
        &self,
        source_type: EvidenceSourceType,
        source_id: i64,
        evidence_id: i64,
    ) -> Result<()> {
        self.with_conn(move |conn| {
            // INSERT OR IGNORE makes this idempotent against the UNIQUE
            // (source_type, source_id, evidence_id) constraint.
            conn.execute(
                "INSERT OR IGNORE INTO knowledge_evidence (source_type, source_id, evidence_id)
         VALUES (?1, ?2, ?3)",
                params![source_type.as_str(), source_id, evidence_id],
            )?;
            Ok(())
        })
        .await
    }

    async fn get_evidence_for(
        &self,
        source_type: EvidenceSourceType,
        source_id: i64,
    ) -> Result<Vec<Evidence>> {
        self.with_conn(move |conn| {
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
        })
        .await
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
        let m = m.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO mentions (object_id, chapter_id, start_offset, end_offset, alias_used, confidence)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![m.object_id, m.chapter_id, m.start_offset, m.end_offset, m.alias_used, m.confidence],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await
    }

    async fn get_mentions_for_object(&self, object_id: i64) -> Result<Vec<Mention>> {
        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM mentions WHERE object_id = ?1 ORDER BY chapter_id ASC")?;
            let rows = stmt.query_map(params![object_id], row_to_mention)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .await
    }

    async fn list_documents(&self) -> Result<Vec<Document>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT * FROM documents ORDER BY id ASC")?;
            let rows = stmt.query_map([], row_to_document)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .await
    }

    async fn list_objects_by_document(&self, doc_id: i64) -> Result<Vec<KnowledgeObject>> {
        self.with_conn(move |conn| {
            let mut stmt =
                conn.prepare("SELECT * FROM knowledge_objects WHERE doc_id = ?1 ORDER BY id ASC")?;
            let rows = stmt.query_map(params![doc_id], row_to_object)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .await
    }

    async fn list_edges_by_document(&self, doc_id: i64) -> Result<Vec<KnowledgeEdge>> {
        self.with_conn(move |conn| {
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
        })
        .await
    }

    async fn list_evidence_by_document(&self, doc_id: i64) -> Result<Vec<Evidence>> {
        self.with_conn(move |conn| {
            let mut stmt =
                conn.prepare("SELECT * FROM evidence WHERE doc_id = ?1 ORDER BY id ASC")?;
            let rows = stmt.query_map(params![doc_id], row_to_evidence)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .await
    }

    async fn list_evidence_links(&self) -> Result<Vec<KnowledgeEvidenceLink>> {
        self.with_conn(|conn| {
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
        })
        .await
    }

    async fn search_objects(
        &self,
        name_contains: Option<&str>,
        object_type: Option<&str>,
        property_contains: Option<&str>,
        doc_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<KnowledgeObject>> {
        // Clamp like search_evidence: an oversized value would wrap to -1, which
        // SQLite treats as "no limit" and materializes the whole table.
        let limit = crate::sql::sql_limit(limit);
        // Escape LIKE wildcards so `%`/`_` in the user's query match literally
        // instead of acting as wildcards; the SQL below concatenates the `%`
        // wildcards itself, so only the body is escaped.
        let name_like = name_contains.map(crate::sql::escape_like);
        let prop_like = property_contains.map(crate::sql::escape_like);
        let object_type = object_type.map(str::to_string);
        self.with_conn(move |conn| {
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
        })
        .await
    }

    async fn graph_counts(&self) -> Result<GraphCounts> {
        self.with_conn(|conn| {
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
        })
        .await
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

    async fn upsert_world_event(&self, event: NewWorldEvent<'_>) -> Result<i64> {
        self.upsert_world_event_row(event).await
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

    async fn list_event_participants(&self, event_id: i64) -> Result<Vec<EventParticipantRef>> {
        self.list_event_participants_row(event_id).await
    }

    async fn upsert_world_state(&self, state: NewWorldState<'_>) -> Result<i64> {
        self.upsert_world_state_row(state).await
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
        let r = r.clone();
        self.with_conn(move |conn| {
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
        })
        .await
    }

    async fn finish_run(
        &self,
        id: i64,
        status: &str,
        statistics: &serde_json::Value,
    ) -> Result<()> {
        let status = status.to_string();
        let statistics = statistics.clone();
        self.with_conn(move |conn| {
            let affected = conn.execute(
                "UPDATE compiler_runs SET status = ?2, finished_at = ?3, statistics = ?4 WHERE id = ?1",
                params![
                    id,
                    status,
                    chrono::Utc::now().timestamp(),
                    json_to_string(&statistics)
                ],
            )?;
            if affected == 0 {
                return Err(StorageError::NotFound(format!("compiler_run {id}")).into());
            }
            Ok(())
        })
        .await
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

    // ── transactions ───────────────────────────────────────────
    async fn begin_transaction(&self) -> Result<()> {
        self.with_conn(|conn| conn.execute_batch("BEGIN;").map_err(Into::into))
            .await
    }

    async fn commit_transaction(&self) -> Result<()> {
        self.with_conn(|conn| conn.execute_batch("COMMIT;").map_err(Into::into))
            .await
    }

    async fn rollback_transaction(&self) -> Result<()> {
        self.with_conn(|conn| conn.execute_batch("ROLLBACK;").map_err(Into::into))
            .await
    }

    async fn in_transaction(&self) -> Result<bool> {
        self.with_conn(|conn| Ok(!conn.is_autocommit())).await
    }
}

impl SQLiteKnowledgeStore {
    /// Resolve an optional document-title filter to an optional doc_id.
    /// Returns `Ok(None)` when no filter is given (match across all docs).
    ///
    /// `pub(super)` because `queries.rs` (a sibling module) is the caller:
    /// privacy is per module, so a private helper here would be invisible to it
    /// once this impl block no longer lives in `mod.rs`.
    pub(super) async fn resolve_doc_id(&self, doc_title: Option<&str>) -> Result<Option<i64>> {
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
    ///
    /// `pub(super)` for the same reason as [`Self::resolve_doc_id`]: the caller
    /// (`queries.rs`) is a sibling module.
    pub(super) async fn resolve_chapter_nos(&self, mentions: &[Mention]) -> Result<Vec<i32>> {
        if mentions.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<i64> = mentions.iter().map(|m| m.chapter_id).collect();
        // Build a parameterized `IN (?, ?, ...)` clause.
        let placeholders: Vec<&str> = ids.iter().map(|_| "?").collect();
        let sql = format!(
            "SELECT id, chapter_no FROM chapters WHERE id IN ({})",
            placeholders.join(", ")
        );
        // The boxed parameter list must be `Send` to cross into the blocking
        // pool, hence the `+ Send` on the trait object.
        let params_vec: Vec<Box<dyn rusqlite::types::ToSql + Send>> = ids
            .iter()
            .map(|id| Box::new(*id) as Box<dyn rusqlite::types::ToSql + Send>)
            .collect();
        let id_to_no: HashMap<i64, i32> = self
            .with_conn(move |conn| {
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map(
                    rusqlite::params_from_iter(params_vec.iter().map(|p| p.as_ref())),
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i32>(1)?)),
                )?;
                let mut id_to_no: HashMap<i64, i32> = HashMap::new();
                for r in rows {
                    let (id, no) = r?;
                    id_to_no.insert(id, no);
                }
                Ok(id_to_no)
            })
            .await?;
        Ok(mentions
            .iter()
            .filter_map(|m| id_to_no.get(&m.chapter_id).copied())
            .collect())
    }
}
