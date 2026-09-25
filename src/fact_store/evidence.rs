//! Evidence anchors: the original text a fact is justified by.
//!
//! A fact is the *evidence unit* of cognitive state, but the utterance it came
//! from is what a human (or an agent) audits — "why do we believe this?".
//! Compilers keep that utterance in `payload.evidence`; this module turns it
//! into a first-class row, so `facts.evidence_id` points at `evidence.content`
//! and `fact_provenance` / `state_timeline` can hand the original text back.

use super::*;

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
    /// Returns `Ok(None)` when the fact carries no anchor.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the insert fails.
    pub(super) fn anchor_evidence_on(
        conn: &Connection,
        payload: &serde_json::Value,
        anchors: &mut std::collections::HashMap<String, i64>,
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
        // Cache key includes the span so two anchors over the same text but at
        // different offsets (e.g. the same sentence cited twice) do not share
        // one row and lose the second position.
        let offset = anchor.get("offset").and_then(serde_json::Value::as_i64);
        let length = anchor.get("length").and_then(serde_json::Value::as_i64);
        let cache_key = format!("{text}\u{0}{offset:?}\u{0}{length:?}");
        if let Some(id) = anchors.get(&cache_key) {
            return Ok(Some(*id));
        }
        // `doc_id: 0` means "not from a corpus document" (the conversation
        // compilers set it for message-sourced utterances), so it is stored as
        // NULL rather than as a document id that cannot exist.
        let doc_id = anchor
            .get("doc_id")
            .and_then(serde_json::Value::as_i64)
            .filter(|id| *id > 0);
        // Persist the span too: the vision requires every claim to trace back
        // to an exact original-text position. Previously only `content` was
        // written and `start_offset`/`end_offset` stayed NULL, so any consumer
        // reading the evidence ROW (not the fact payload) lost the anchor.
        conn.execute(
            "INSERT INTO evidence (doc_id, chapter_id, start_offset, end_offset, content)
             VALUES (?1, NULL, ?2, ?3, ?4)",
            params![
                doc_id,
                offset,
                length.map(|l| offset.unwrap_or(0) + l),
                text
            ],
        )?;
        let id = conn.last_insert_rowid();
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

    /// Insert an original-text evidence row and return its id.
    ///
    /// Facts reference evidence via `evidence_id`; this is the explicit write
    /// path for callers that own the anchor text themselves (imports, tests).
    /// Compiled conversation facts are anchored automatically by
    /// [`SqliteFactStore::anchor_evidence_on`].
    ///
    /// # Errors
    ///
    /// Returns a storage error when the insert fails.
    pub fn insert_evidence(
        &self,
        doc_id: Option<i64>,
        chapter_id: Option<i64>,
        content: &str,
    ) -> Result<i64> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO evidence (doc_id, chapter_id, content) VALUES (?1, ?2, ?3)",
            params![doc_id, chapter_id, content],
        )?;
        Ok(conn.last_insert_rowid())
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
}
