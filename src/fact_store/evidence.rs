//! Evidence anchors: the original text a fact is justified by.
//!
//! A fact is the *evidence unit* of cognitive state, but the utterance it came
//! from is what a human (or an agent) audits — "why do we believe this?".
//! Compilers keep that utterance in `payload.evidence`; this module turns it
//! into a first-class row, so `facts.evidence_id` points at `evidence.content`
//! and `fact_provenance` / `state_timeline` can hand the original text back.

use super::*;

/// One evidence row as the provenance API reads it: the anchor text plus
/// the source byte span it was recorded with.
#[derive(Debug, Clone)]
pub struct EvidenceAnchor {
    /// Anchor text (`None` only for a legacy row with NULL content).
    pub content: Option<String>,
    /// Source byte span start, when the anchor recorded one.
    pub start_offset: Option<i64>,
    /// Source byte span end, when the anchor recorded one.
    pub end_offset: Option<i64>,
}

impl SqliteFactStore {
    /// Register the original-text anchor a compiled fact carries in
    /// `payload["evidence"]` and return its row id.
    ///
    /// Compilers keep the anchor inside the payload
    /// (`{doc_id, offset, length, text}`) while the fact is inserted with
    /// `evidence_id = NULL`. Without this step the `evidence` table stays empty
    /// in production: `fact_provenance`'s "why do we believe this?" answers
    /// `null` for every real fact, and `state_timeline` never reports an
    /// `evidence_ids` entry even though the plan requires each state to carry
    /// its evidence.
    ///
    /// `tenant_id` is the owner of the fact's entity (the caller resolves it
    /// with [`SqliteFactStore::tenant_of_on`]); it is written explicitly so the
    /// anchor belongs to the same tenant as the fact it justifies — every row
    /// used to land in the column's `'default'`, where no per-tenant export or
    /// deletion could attribute it.
    ///
    /// Returns `Ok(None)` when the fact carries no anchor.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the insert fails.
    pub(super) fn anchor_evidence_on(
        conn: &Connection,
        payload: &serde_json::Value,
        anchors: &mut std::collections::HashMap<String, i64>,
        tenant_id: &str,
    ) -> Result<Option<i64>> {
        let Some(anchor) = payload.get("evidence") else {
            return Ok(None);
        };
        let Some(text) = anchor.get("text").and_then(serde_json::Value::as_str) else {
            return Ok(None);
        };
        if text.is_empty() {
            return Ok(None);
        }
        // Cache key mirrors the ROW's identity, not just its text: the same
        // sentence at two offsets is two anchors, and so is the same span under
        // another tenant or in another document. Keying on text+span alone
        // would hand one tenant's row to another and lose the second position
        // (the database distinguishes all of it, so a laxer cache only creates
        // silent cross-links).
        let offset = anchor.get("offset").and_then(serde_json::Value::as_i64);
        let length = anchor.get("length").and_then(serde_json::Value::as_i64);
        // `doc_id: 0` means "not from a corpus document" (the conversation
        // compilers set it for message-sourced utterances), so it is stored as
        // NULL rather than as a document id that cannot exist.
        let doc_id = anchor
            .get("doc_id")
            .and_then(serde_json::Value::as_i64)
            .filter(|id| *id > 0);
        let cache_key =
            format!("{tenant_id}\u{0}{doc_id:?}\u{0}{text}\u{0}{offset:?}\u{0}{length:?}");
        if let Some(id) = anchors.get(&cache_key) {
            return Ok(Some(*id));
        }
        // Persist the span too: the vision requires every claim to trace back
        // to an exact original-text position. Previously only `content` was
        // written and `start_offset`/`end_offset` stayed NULL, so any consumer
        // reading the evidence ROW (not the fact payload) lost the anchor.
        //
        // A length without an offset is not a span: writing `0 + length` would
        // invent a position nobody recorded, so the end stays NULL.
        let end_offset = offset.zip(length).map(|(start, len)| start + len);
        // Idempotent by identity (see `UNIQUE_INDEXES` in `fact_store`): the
        // same anchor is one row however often the conversation is compiled.
        // `RETURNING id` and not `last_insert_rowid()` — the latter is NOT
        // updated when the insert turns into the `DO UPDATE` branch, so it
        // would hand back whatever row this connection inserted last.
        let id: i64 = conn.query_row(
            "INSERT INTO evidence \
             (doc_id, tenant_id, chapter_id, start_offset, end_offset, content) \
             VALUES (?1, ?2, NULL, ?3, ?4, ?5) \
             ON CONFLICT(tenant_id, IFNULL(doc_id, -1), IFNULL(chapter_id, -1), \
                         IFNULL(start_offset, -1), IFNULL(end_offset, -1), \
                         IFNULL(content, '')) \
             DO UPDATE SET content = excluded.content \
             RETURNING id",
            params![doc_id, tenant_id, offset, end_offset, text],
            |row| row.get(0),
        )?;
        anchors.insert(cache_key, id);
        Ok(Some(id))
    }

    /// Fetch the original-text evidence row behind a fact's `evidence_id`.
    ///
    /// Returns `None` when the fact has no evidence anchor or the row vanished.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the read fails.
    pub fn get_evidence_content(&self, evidence_id: i64) -> Result<Option<String>> {
        let conn = self.lock_conn()?;
        let content = conn
            .query_row(
                "SELECT content FROM evidence WHERE id = ?1",
                params![evidence_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        Ok(content.flatten())
    }

    /// Read an evidence row's content AND its source byte span.
    ///
    /// `get_evidence_content` returns the text only, which broke the
    /// re-locatability contract at the API surface: `fact_provenance` could
    /// quote the anchor but not say WHERE in the original text it sits.
    /// Returns `Ok(None)` when no row with that id exists.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the read fails.
    pub fn get_evidence_anchor(&self, evidence_id: i64) -> Result<Option<EvidenceAnchor>> {
        let conn = self.lock_conn()?;
        let row = conn
            .query_row(
                "SELECT content, start_offset, end_offset FROM evidence WHERE id = ?1",
                params![evidence_id],
                |row| {
                    Ok(EvidenceAnchor {
                        content: row.get(0)?,
                        start_offset: row.get(1)?,
                        end_offset: row.get(2)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Insert an original-text evidence row and return its id.
    ///
    /// Facts reference evidence via `evidence_id`; this is the explicit write
    /// path for callers that own the anchor text themselves (imports, tests).
    /// Compiled conversation facts are anchored automatically by
    /// [`SqliteFactStore::anchor_evidence_on`], which resolves the tenant from
    /// the fact's entity.
    ///
    /// `tenant_id` is required rather than defaulted: the column has a
    /// `'default'` default, and rows that silently land there cannot be
    /// attributed to an owner afterwards.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the insert fails.
    pub fn insert_evidence(
        &self,
        tenant_id: &str,
        doc_id: Option<i64>,
        chapter_id: Option<i64>,
        content: &str,
    ) -> Result<i64> {
        let conn = self.lock_conn()?;
        // Same identity as `anchor_evidence_on`: writing the anchor a second
        // time reuses the row instead of appending a duplicate.
        conn.query_row(
            "INSERT INTO evidence (tenant_id, doc_id, chapter_id, content) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(tenant_id, IFNULL(doc_id, -1), IFNULL(chapter_id, -1), \
                         IFNULL(start_offset, -1), IFNULL(end_offset, -1), \
                         IFNULL(content, '')) \
             DO UPDATE SET content = excluded.content \
             RETURNING id",
            params![tenant_id, doc_id, chapter_id, content],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fact with no payload anchor, used to prove nothing is invented.
    fn plain_fact(entity_id: i64, fact_type: FactType) -> Fact {
        Fact {
            id: None,
            entity_id,
            fact_type,
            time: 2026,
            payload: serde_json::json!({"content": "无关事实"}),
            evidence_id: None,
            created_at: 2026,
            ..Fact::default()
        }
    }

    /// Objective: Verify `get_evidence_anchor` reads back the SPAN written by
    /// `anchor_evidence_on` — content-only reads (`get_evidence_content`)
    /// broke re-locatability at the `fact_provenance` API surface.
    /// Invariants: content matches; start == offset; end == offset + length;
    /// an unknown id yields None.
    #[test]
    fn evidence_anchor_round_trips_the_source_span() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        let payload = serde_json::json!({
            "evidence": {
                "doc_id": 0,
                "offset": 12,
                "length": 8,
                "text": "我从去年开始喜欢 Rust"
            }
        });
        let mut anchors = std::collections::HashMap::new();
        let id = {
            let conn = store.lock_conn().expect("lock conn");
            SqliteFactStore::anchor_evidence_on(&conn, &payload, &mut anchors, "tenant-a")
                .expect("anchor")
                .expect("anchor row created")
        };

        let anchor = store
            .get_evidence_anchor(id)
            .expect("read anchor")
            .expect("row exists");
        assert_eq!(
            anchor.content.as_deref(),
            Some("我从去年开始喜欢 Rust"),
            "content round-trips"
        );
        assert_eq!(anchor.start_offset, Some(12), "span start round-trips");
        assert_eq!(anchor.end_offset, Some(20), "span end is offset + length");

        assert!(
            store
                .get_evidence_anchor(999_999)
                .expect("missing id is not an error")
                .is_none(),
            "unknown id yields None"
        );
    }

    /// Objective: Verify a compiled fact's payload anchor becomes a real
    /// `evidence` row linked through `evidence_id`. Compilers keep the original
    /// text in `payload.evidence` and nothing ever wrote the table, so
    /// `fact_provenance`'s "why do we believe this?" answered `null` for every
    /// real fact and `state_timeline` intervals never carried an `evidence_ids`
    /// entry.
    /// Invariants: the two facts compiled from ONE utterance share a single row;
    /// that row is readable through `get_evidence_content`; a `doc_id: 0` anchor
    /// is stored as NULL; a fact without a payload anchor stays unanchored.
    #[test]
    fn compiled_facts_register_their_evidence_anchor() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        let anchored = |fact_type: FactType| Fact {
            id: None,
            entity_id: 100,
            fact_type,
            time: 2026,
            payload: serde_json::json!({
                "content": "我喜欢独处",
                "evidence": {
                    "doc_id": 0,
                    "offset": 0,
                    "length": 2,
                    "text": "我喜欢独处",
                },
            }),
            evidence_id: None,
            created_at: 2026,
            ..Fact::default()
        };
        store
            .insert_batch(&[anchored(FactType::Preference), anchored(FactType::Goal)])
            .expect("insert anchored facts");

        let stored = store.get_facts(100).expect("read facts");
        assert_eq!(stored.len(), 2, "both anchored facts are stored");
        let anchors: Vec<i64> = stored
            .iter()
            .map(|fact| {
                fact.evidence_id
                    .expect("every compiled fact must gain an evidence anchor")
            })
            .collect();
        assert_eq!(
            anchors[0], anchors[1],
            "the facts of ONE utterance must share a single evidence row"
        );
        assert_eq!(
            store
                .get_evidence_content(anchors[0])
                .expect("read the evidence row")
                .as_deref(),
            Some("我喜欢独处"),
            "the original text must be retrievable from the evidence table"
        );

        let conn = store.lock_conn().expect("lock the fact database");
        let doc_id: Option<i64> = conn
            .query_row(
                "SELECT doc_id FROM evidence WHERE id = ?1",
                params![anchors[0]],
                |row| row.get(0),
            )
            .expect("read the evidence row");
        drop(conn);
        assert_eq!(
            doc_id, None,
            "`doc_id: 0` means `not from a document` and must be stored as NULL"
        );

        let plain = store
            .insert_fact(&plain_fact(200, FactType::Event))
            .expect("insert a fact without an anchor");
        assert_eq!(
            store
                .get_fact_by_id(plain)
                .expect("read the plain fact")
                .expect("the plain fact exists")
                .evidence_id,
            None,
            "a fact without a payload anchor must not invent one"
        );
    }

    /// Build a fact of `entity_id` carrying a payload anchor with `text`.
    fn anchored_fact(entity_id: i64, text: &str) -> Fact {
        Fact {
            id: None,
            entity_id,
            fact_type: FactType::Preference,
            time: 2026,
            payload: serde_json::json!({
                "content": text,
                "evidence": {"doc_id": 0, "offset": 0, "length": 2, "text": text},
            }),
            evidence_id: None,
            created_at: 2026,
            ..Fact::default()
        }
    }

    /// Objective: Verify an evidence anchor inherits the tenant that owns the
    /// fact's entity. Every row used to be inserted with the column default
    /// (`'default'`), so no per-tenant export, deletion or decay could attribute
    /// — or even find — the text that justifies a fact.
    /// Invariants: a fact owned by `tenant-a` writes a `tenant-a` anchor; a fact
    /// whose entity row is absent writes an empty tenant rather than the
    /// misleading default.
    #[test]
    fn evidence_anchor_inherits_the_entity_tenant() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        let alice = store
            .resolve_user("tenant-a", "alice")
            .expect("resolve the user entity");
        store
            .insert_batch(&[anchored_fact(alice, "我喜欢独处")])
            .expect("insert the tenant's anchored fact");
        store
            .insert_batch(&[anchored_fact(9_999, "没有实体锚点")])
            .expect("insert a fact whose entity row does not exist");

        let conn = store.lock_conn().expect("lock conn");
        let owned: String = conn
            .query_row(
                "SELECT tenant_id FROM evidence ORDER BY id ASC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("read the owned anchor");
        assert_eq!(
            owned, "tenant-a",
            "the anchor must inherit the fact's tenant, not the column default"
        );
        let orphan: String = conn
            .query_row(
                "SELECT tenant_id FROM evidence ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("read the orphan anchor");
        assert_eq!(
            orphan, "",
            "an entity that is gone yields no tenant — never a borrowed one"
        );
    }

    /// Objective: Verify a half-recorded span is not completed by arithmetic.
    /// `offset.unwrap_or(0) + length` used to write `end = length` for an anchor
    /// that carried a length but no offset, inventing a position nobody
    /// recorded — an anchor that looks re-locatable and is not.
    /// Invariants: no offset → both ends stay NULL; a full span round-trips.
    #[test]
    fn half_a_span_is_not_invented() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        let insert = |payload: serde_json::Value| {
            let fact = Fact {
                payload,
                ..anchored_fact(7, "半截偏移")
            };
            store.insert_fact(&fact).expect("insert anchored fact");
        };
        insert(serde_json::json!({
            "content": "半截偏移",
            "evidence": {"doc_id": 0, "length": 8, "text": "半截偏移"},
        }));
        insert(serde_json::json!({
            "content": "半截偏移",
            "evidence": {"doc_id": 0, "offset": 12, "length": 8, "text": "半截偏移"},
        }));

        let conn = store.lock_conn().expect("lock conn");
        let spans: Vec<(Option<i64>, Option<i64>)> = {
            let mut stmt = conn
                .prepare("SELECT start_offset, end_offset FROM evidence ORDER BY id ASC")
                .expect("prepare");
            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .expect("query spans");
            rows.map(|row| row.expect("row")).collect()
        };
        assert_eq!(
            spans,
            vec![(None, None), (Some(12), Some(20))],
            "a length without an offset is not a span, and a full span keeps its end"
        );
    }
    /// Objective: Verify the same anchor is ONE row however often the
    /// conversation is compiled. `evidence` had no identity at all, so every
    /// compile appended another copy of every anchor: the table grew with the
    /// compile count while nothing about the facts it justifies changed.
    /// Invariants: re-anchoring the same span reuses the row; a different span,
    /// document or tenant still gets its own row.
    #[test]
    fn anchoring_the_same_span_reuses_the_row() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        let payload = serde_json::json!({
            "evidence": { "doc_id": 0, "offset": 12, "length": 8, "text": "我从去年开始喜欢 Rust" }
        });
        let shifted = serde_json::json!({
            "evidence": { "doc_id": 0, "offset": 40, "length": 8, "text": "我从去年开始喜欢 Rust" }
        });
        let conn = store.lock_conn().expect("lock connection");
        let mut anchors = std::collections::HashMap::new();

        let first = SqliteFactStore::anchor_evidence_on(&conn, &payload, &mut anchors, "default")
            .expect("anchor")
            .expect("row created");
        let mut next_call = std::collections::HashMap::new();
        let second =
            SqliteFactStore::anchor_evidence_on(&conn, &payload, &mut next_call, "default")
                .expect("anchor again")
                .expect("row reused");
        assert_eq!(
            first, second,
            "re-compiling the same conversation must reuse its anchor row"
        );

        let other_span =
            SqliteFactStore::anchor_evidence_on(&conn, &shifted, &mut next_call, "default")
                .expect("anchor")
                .expect("row created");
        let other_tenant = SqliteFactStore::anchor_evidence_on(&conn, &payload, &mut anchors, "t2")
            .expect("anchor")
            .expect("row created");
        assert_ne!(other_span, first, "a different span is a different anchor");
        assert_ne!(
            other_tenant, first,
            "the same span under another tenant is a different anchor"
        );

        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM evidence", [], |row| row.get(0))
            .expect("count anchors");
        assert_eq!(rows, 3, "three identities, three rows");
    }
}
