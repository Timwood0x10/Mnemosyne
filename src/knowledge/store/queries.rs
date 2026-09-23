//! Query projections: entity views, timelines, relation graphs and evidence
//! search. Kept as inherent methods so the single `KnowledgeStore` trait impl
//! can stay in `mod.rs` (Rust forbids splitting one trait impl across files).

use super::*;

impl SQLiteKnowledgeStore {
    pub(super) async fn update_object_properties_query(
        &self,
        id: i64,
        properties: &serde_json::Value,
        confidence: Option<f64>,
    ) -> Result<usize> {
        // Read-modify-write runs inside a SINGLE lock critical section (the
        // conn guard is held across SELECT and UPDATE), so concurrent callers
        // serialize and no lost update occurs.
        let conn = self.conn.lock().await;
        let existing: Option<(String, f64)> = conn
            .query_row(
                "SELECT properties, confidence FROM knowledge_objects WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((raw, stored_confidence)) = existing else {
            return Ok(0);
        };
        let mut merged = serde_json::from_str::<serde_json::Value>(&raw)
            .unwrap_or_else(|_| serde_json::json!({}));
        if let (Some(merged_obj), Some(new_obj)) = (merged.as_object_mut(), properties.as_object())
        {
            for (k, v) in new_obj {
                merged_obj.insert(k.clone(), v.clone());
            }
        }
        // `None` keeps the stored confidence (properties-only merge must not
        // clobber it with a hardcoded value); `Some(c)` raises/lowers it.
        let new_confidence = confidence.unwrap_or(stored_confidence);
        let n = conn.execute(
            "UPDATE knowledge_objects SET properties = ?1, confidence = ?2 WHERE id = ?3",
            params![json_to_string(&merged), new_confidence, id],
        )?;
        Ok(n)
    }

    pub(super) async fn find_object_by_alias_query(
        &self,
        name: &str,
        doc_id: Option<i64>,
    ) -> Result<Option<KnowledgeObject>> {
        // Exact match is authoritative — never substitute a substring hit when
        // an exact object exists (exact semantics unchanged for existing callers).
        if let Some(exact) = self.find_object_by_name(name, doc_id).await? {
            return Ok(Some(exact));
        }
        // Substring fallback, matched in BOTH directions so a full-name query
        // ("白流苏") finds a stored given name ("流苏") and a given-name query
        // ("流苏") finds a stored full name ("白流苏"). We enumerate the
        // document's objects and keep every one where either name contains the
        // other; a SINGLE unambiguous candidate is required so an alias never
        // silently maps to the wrong person. Exact-name echoes are excluded
        // (already handled above).
        //
        // When no doc is given, match at the SQL layer instead of pulling the
        // first 10_000 objects via search_objects — a large graph with >10k
        // objects silently failed to resolve aliases beyond the cap.
        // LIKE wildcards in the query are escaped so `%`/`_` match literally.
        let escaped = name
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let candidates: Vec<KnowledgeObject> = match doc_id {
            Some(d) => self.list_objects_by_document(d).await?,
            None => {
                let conn = self.conn.lock().await;
                let mut stmt = conn.prepare(
                    "SELECT * FROM knowledge_objects \
                     WHERE name != ?1 \
                       AND (name LIKE '%' || ?2 || '%' ESCAPE '\\' \
                            OR ?3 LIKE '%' || name || '%' ESCAPE '\\') \
                     ORDER BY id ASC",
                )?;
                let rows = stmt.query_map(params![name, escaped, escaped], row_to_object)?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                out
            }
        };
        let matched: Vec<KnowledgeObject> = candidates
            .into_iter()
            .filter(|o| o.name != name && (o.name.contains(name) || name.contains(&o.name)))
            .collect();
        if matched.len() == 1 {
            // Exactly one candidate: a safe, unambiguous alias hit. The
            // len==1 guard guarantees the next() below is Some, so the
            // unwrap cannot fail (justified per error-handling rules).
            Ok(matched.into_iter().next())
        } else {
            // Multiple matches: a person query ("白流苏") whose substring also
            // appears inside sentence-named Event objects should resolve to the
            // person. But this is only safe when exactly ONE Person matches —
            // several persons sharing the substring remain genuinely ambiguous.
            let persons: Vec<KnowledgeObject> = matched
                .into_iter()
                .filter(|o| o.object_type == ObjectType::Person)
                .collect();
            if persons.len() == 1 {
                Ok(persons.into_iter().next())
            } else {
                Ok(None)
            }
        }
    }

    pub(super) async fn get_evidence_for_many_query(
        &self,
        source_type: EvidenceSourceType,
        source_ids: &[i64],
    ) -> Result<Vec<Evidence>> {
        if source_ids.is_empty() {
            return Ok(Vec::new());
        }
        // One IN-clause query replaces N per-edge lookups (inspect_entity
        // previously called get_evidence_for once per relation edge).
        let placeholders = vec!["?"; source_ids.len()].join(",");
        let sql = format!(
            "SELECT e.* FROM evidence e
             JOIN knowledge_evidence ke ON ke.evidence_id = e.id
             WHERE ke.source_type = ?1 AND ke.source_id IN ({placeholders})
             ORDER BY e.id ASC"
        );
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        // First param is the source_type string, then the id list.
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(source_type.as_str().to_string())];
        for id in source_ids {
            params_vec.push(Box::new(*id));
        }
        let rows = stmt.query_map(
            rusqlite::params_from_iter(params_vec.iter()),
            row_to_evidence,
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub(super) async fn inspect_entity_query(
        &self,
        name: &str,
        doc_title: Option<&str>,
    ) -> Result<Option<InspectEntityResult>> {
        let doc_id = self.resolve_doc_id(doc_title).await?;
        // Alias-aware lookup so a corpus-discovered given name ("流苏") is
        // reachable by the full name ("白流苏") and vice versa.
        let object = match self.find_object_by_alias(name, doc_id).await? {
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

        // Events: one bulk fetch instead of one `get_object` per event id.
        let event_ids_vec: Vec<i64> = event_ids.iter().copied().collect();
        let events: Vec<KnowledgeObject> = self.get_objects_bulk(&event_ids_vec).await?;

        // Evidences: union of object evidence + each edge's evidence, deduped.
        // One bulk fetch for all edges replaces one `get_evidence_for` per edge.
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
        let edge_ids: Vec<i64> = edges.iter().map(|e| e.id).collect();
        for ev in self
            .get_evidence_for_many(EvidenceSourceType::Edge, &edge_ids)
            .await?
        {
            if seen_ev.insert(ev.id) {
                evidences.push(ev);
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
            //
            // NO bare single characters: `死` matched "死战不退"/"生死之交"
            // and `卒` matched "士卒" — false death flags. Only multi-char
            // death phrases count (mirrors ingest's DEATH_KW fix).
            let death_kws = [
                "战死", "去世", "身亡", "阵亡", "死亡", "病逝", "病故", "殒命", "毙命", "驾崩",
                "圆寂", "陨落",
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
            // The store layer is link-unaware; the MCP `inspect_entity` tool
            // populates this field from the attached EntityLinker so the
            // cross-source aliases ride on the same response payload
            // (external-knowledge-plan §D3).
            external_aliases: Vec::new(),
        }))
    }

    pub(super) async fn entity_timeline_query(
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
                // Keep NULL valid_from as None (NEW-K12) — the old
                // `unwrap_or(0)` masked unknown chapters as a fake "chapter 0".
                chapter: e.valid_from,
                event: event_label,
                predicate: e.predicate.clone(),
                target,
            });
        }
        // Stable order by chapter; ties keep insertion (valid_from ASC) order.
        entries.sort_by_key(|t| t.chapter);
        Ok(entries)
    }

    pub(super) async fn relation_graph_query(
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

    pub(super) async fn search_evidence_query(
        &self,
        query: &str,
        doc_title: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EvidenceHit>> {
        let conn = self.conn.lock().await;
        // Clamp to avoid `usize::MAX as i64` overflow (which becomes -1 and is
        // treated as "no limit" by SQLite) and to bound memory use.
        let limit = limit.min(10_000) as i64;
        // Escape the backslash FIRST (so the `\%`/`\_` inserted below are not
        // re-escaped), then the LIKE wildcards. Without the backslash escape,
        // a query containing `\` (e.g. `C:\`) produced a malformed pattern
        // where the trailing `\` swallowed the closing `%` (audit finding).
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let like = format!("%{escaped}%");
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
