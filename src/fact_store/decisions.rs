//! Decision persistence: the store for what the agent decided, why it
//! was made (supporting facts, not causality), and the observed outcome.

use super::*;

impl SqliteFactStore {
    /// Insert a decision and return its id.
    ///
    /// `because` is stored as a JSON array of supporting fact ids (supporting
    /// evidence, not causality).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when the decision violates
    /// [`crate::decision::validate_decision`] (empty or oversized fields), and a
    /// storage error when the insert fails.
    pub fn insert_decision(&self, decision: &crate::decision::Decision) -> Result<i64> {
        Self::validate_decision_for_write(decision)?;
        let conn = self.lock_conn()?;
        Self::insert_decision_on(&conn, decision)
    }

    /// Persist one compilation atomically: the compiled facts, the fact that
    /// anchors each commitment, and the decisions themselves.
    ///
    /// Every write happens inside ONE transaction. The previous compile path
    /// wrote the facts first and the decisions afterwards, so a rejected
    /// decision (or a failed agent lookup) returned an error while the facts —
    /// and sometimes an orphan anchor — stayed in the store, and a client retry
    /// duplicated them.
    ///
    /// Each entry of `commitments` pairs a commitment with the anchor fact that
    /// represents it. The anchor is inserted before `Decision::because` is
    /// resolved, so the supporting-fact scan sees exactly the rows it would have
    /// seen on the old two-step path (this turn's facts included, the anchor
    /// itself excluded because `because` links to it explicitly).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when any decision violates
    /// `validate_decision` — checked before the first write, so a bad decision
    /// leaves the store untouched — and a storage error when the transaction
    /// fails.
    pub(crate) fn insert_compilation(
        &self,
        facts: &[Fact],
        commitments: &[(Fact, crate::decision::Decision)],
    ) -> Result<(usize, usize)> {
        for (_, decision) in commitments {
            Self::validate_decision_for_write(decision)?;
        }
        let mut conn = self.lock_conn()?;
        let transaction = conn.transaction()?;
        // One evidence cache for the whole compilation: the several facts of a
        // single utterance share their anchor row.
        let mut anchors = std::collections::HashMap::new();
        Self::insert_facts_on(&transaction, facts, &mut anchors)?;
        let mut stored = facts.len();
        for (anchor, decision) in commitments {
            // Read the subject's facts BEFORE the anchor lands, so `because`
            // never lists the anchor twice.
            let existing = Self::get_facts_on(&transaction, decision.subject)?;
            Self::insert_facts_on(&transaction, std::slice::from_ref(anchor), &mut anchors)?;
            let anchor_id = transaction.last_insert_rowid();
            stored += 1;
            let mut linked = decision.clone();
            linked.because = std::iter::once(anchor_id)
                .chain(crate::commitment::supporting_fact_ids(
                    &existing,
                    decision.subject,
                    &decision.object,
                ))
                .collect();
            Self::insert_decision_on(&transaction, &linked)?;
        }
        transaction.commit()?;
        Ok((stored, commitments.len()))
    }

    /// Reject a decision that `validate_decision` refuses, before any write.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] naming the offending field.
    fn validate_decision_for_write(decision: &crate::decision::Decision) -> Result<()> {
        match crate::decision::validate_decision(decision) {
            Some(field) => Err(Error::InvalidInput(format!(
                "invalid decision field `{field}`"
            ))),
            None => Ok(()),
        }
    }

    /// Insert a decision on an already-locked connection.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the insert fails.
    fn insert_decision_on(conn: &Connection, decision: &crate::decision::Decision) -> Result<i64> {
        let because = serde_json::to_string(&decision.because)?;
        conn.execute(
            "INSERT INTO decisions (subject, verb, object, made_at, because, outcome, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                decision.subject,
                decision.verb,
                decision.object,
                decision.made_at,
                because,
                decision
                    .outcome
                    .map(crate::decision::DecisionOutcome::as_str),
                decision.status.as_str(),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Fetch a single decision by id, or `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the read fails.
    pub fn get_decision(&self, decision_id: i64) -> Result<Option<crate::decision::Decision>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, subject, verb, object, made_at, because, outcome, status
             FROM decisions WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![decision_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::row_to_decision(row)?)),
            None => Ok(None),
        }
    }

    /// Fetch every decision made by an entity, newest first.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the read fails.
    pub fn get_decisions(&self, subject: i64) -> Result<Vec<crate::decision::Decision>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, subject, verb, object, made_at, because, outcome, status
             FROM decisions WHERE subject = ?1 ORDER BY made_at DESC, id DESC",
        )?;
        let mut rows = stmt.query(params![subject])?;
        let mut decisions = Vec::new();
        while let Some(row) = rows.next()? {
            decisions.push(Self::row_to_decision(row)?);
        }
        Ok(decisions)
    }

    /// Search decisions by keyword over `verb`/`object` (case-insensitive),
    /// newest first. Lightweight companion-search: decisions deliberately do
    /// not depend on the embedding-based retrieval engine.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the read fails.
    pub fn search_decisions(
        &self,
        subject: i64,
        keyword: &str,
    ) -> Result<Vec<crate::decision::Decision>> {
        // Escape the backslash FIRST (so the `\%`/`\_` inserted below are not
        // re-escaped), then the LIKE wildcards: without this a keyword such as
        // `%` matched every decision and `_` acted as a single-character
        // wildcard (mirrors `knowledge/store.rs::search_evidence`).
        let escaped = keyword
            .to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, subject, verb, object, made_at, because, outcome, status
             FROM decisions
             WHERE subject = ?1
               AND (lower(verb) LIKE ?2 ESCAPE '\\' OR lower(object) LIKE ?2 ESCAPE '\\')
             ORDER BY made_at DESC, id DESC",
        )?;
        let mut rows = stmt.query(params![subject, pattern])?;
        let mut decisions = Vec::new();
        while let Some(row) = rows.next()? {
            decisions.push(Self::row_to_decision(row)?);
        }
        Ok(decisions)
    }

    /// Update a decision's outcome and close it. Applies the outcome exactly
    /// once (see [`crate::decision::apply_outcome`]).
    ///
    /// The "exactly once" guard lives in the `UPDATE` statement itself
    /// (`AND outcome IS NULL`), so two concurrent callers can never overwrite
    /// each other: a read-then-write pair would let the second caller replace
    /// the first recorded outcome. Returns `None` when the id is unknown.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the update or the read-back fails.
    pub fn set_decision_outcome(
        &self,
        decision_id: i64,
        outcome: crate::decision::DecisionOutcome,
    ) -> Result<Option<crate::decision::Decision>> {
        // Scope the connection guard: it must be dropped BEFORE the read-back
        // below, which re-locks the connection. `lock_conn()` is a non-reentrant
        // Mutex, so holding the guard across `get_decision` would deadlock.
        {
            let conn = self.lock_conn()?;
            Self::apply_outcome_once(&conn, decision_id, outcome)?;
        }
        self.get_decision(decision_id)
    }

    /// Record `outcome` only when the decision has none yet.
    ///
    /// Returns the number of rows changed: `1` for the first recorded outcome,
    /// `0` when the decision is unknown or already closed. The condition is part
    /// of the statement, making the check and the write a single atomic step.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the update fails.
    fn apply_outcome_once(
        conn: &Connection,
        decision_id: i64,
        outcome: crate::decision::DecisionOutcome,
    ) -> Result<usize> {
        let changed = conn.execute(
            "UPDATE decisions SET outcome = ?1, status = 'closed'
             WHERE id = ?2 AND outcome IS NULL",
            params![outcome.as_str(), decision_id],
        )?;
        Ok(changed)
    }

    /// Decode a decision row into a [`crate::decision::Decision`].
    ///
    /// # Errors
    ///
    /// Returns an invalid-data error when the `because` column is not a JSON
    /// array of integer ids.
    fn row_to_decision(row: &rusqlite::Row<'_>) -> Result<crate::decision::Decision> {
        let because_text: String = row.get("because")?;
        let because: Vec<i64> = serde_json::from_str(&because_text).map_err(|error| {
            Error::Storage(StorageError::InvalidData(format!(
                "decision because is not a valid id array: {error}"
            )))
        })?;
        let outcome_raw: Option<String> = row.get("outcome")?;
        let outcome = outcome_raw
            .as_deref()
            .and_then(crate::decision::DecisionOutcome::parse);
        Ok(crate::decision::Decision {
            id: Some(row.get("id")?),
            subject: row.get("subject")?,
            verb: row.get("verb")?,
            object: row.get("object")?,
            made_at: row.get("made_at")?,
            because,
            outcome,
            status: crate::decision::DecisionStatus::parse(&row.get::<_, String>("status")?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Decision CRUD ─────────────────────────────────────────────────────

    fn sample_decision(subject: i64, verb: &str, object: &str) -> crate::decision::Decision {
        crate::decision::Decision {
            id: None,
            subject,
            verb: verb.to_string(),
            object: object.to_string(),
            made_at: 2026,
            because: vec![17, 23],
            outcome: None,
            status: crate::decision::DecisionStatus::Open,
        }
    }

    /// Objective: Verify a decision round-trips insert → read, preserving the
    /// supporting-evidence chain and the open status.
    /// Invariants: id assigned; because/verb/object/made_at survive; status is
    /// Open with no outcome.
    #[test]
    fn decision_insert_and_read_roundtrip() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        let id = store
            .insert_decision(&sample_decision(7, "promise", "陪用户明天去医院"))
            .expect("insert decision");
        let read = store
            .get_decision(id)
            .expect("read decision")
            .expect("decision exists");
        assert_eq!(read.subject, 7);
        assert_eq!(read.verb, "promise");
        assert_eq!(read.object, "陪用户明天去医院");
        assert_eq!(read.because, vec![17, 23], "supporting facts preserved");
        assert_eq!(read.status, crate::decision::DecisionStatus::Open);
        assert_eq!(read.outcome, None, "outcome starts empty");
    }

    /// Objective: Verify the decision list is newest-first per subject and
    /// that an unknown id reads as None (not an error).
    /// Invariants: later made_at sorts first; unknown id → None.
    #[test]
    fn decisions_list_newest_first_and_unknown_id_is_none() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        let mut old = sample_decision(7, "promise", "旧承诺");
        old.made_at = 2024;
        let mut fresh = sample_decision(7, "decide", "新决定");
        fresh.made_at = 2026;
        store.insert_decision(&old).expect("insert old");
        store.insert_decision(&fresh).expect("insert fresh");
        // A different subject's decision must not leak into entity 7's list.
        let other = sample_decision(8, "decline", "别人的决定");
        store.insert_decision(&other).expect("insert other");

        let decisions = store.get_decisions(7).expect("list decisions");
        assert_eq!(decisions.len(), 2, "only subject 7 decisions returned");
        assert_eq!(decisions[0].object, "新决定", "newest first");
        assert_eq!(decisions[1].object, "旧承诺");
        assert!(
            store.get_decision(9999).expect("read").is_none(),
            "unknown id reads as None"
        );
    }

    /// Objective: Verify `set_decision_outcome` records the outcome once and
    /// closes the decision; the second call is a no-op (can't be both
    /// fulfilled and violated).
    /// Invariants: first outcome wins; status → Closed; second call keeps the
    /// first outcome; unknown id returns None.
    #[test]
    fn decision_outcome_applies_exactly_once() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        let id = store
            .insert_decision(&sample_decision(7, "promise", "明天去医院"))
            .expect("insert decision");

        let closed = store
            .set_decision_outcome(id, crate::decision::DecisionOutcome::Fulfilled)
            .expect("apply outcome")
            .expect("decision exists");
        assert_eq!(
            closed.outcome,
            Some(crate::decision::DecisionOutcome::Fulfilled)
        );
        assert_eq!(closed.status, crate::decision::DecisionStatus::Closed);

        let again = store
            .set_decision_outcome(id, crate::decision::DecisionOutcome::Violated)
            .expect("apply again")
            .expect("decision exists");
        assert_eq!(
            again.outcome,
            Some(crate::decision::DecisionOutcome::Fulfilled),
            "first outcome wins"
        );

        assert!(
            store
                .set_decision_outcome(9999, crate::decision::DecisionOutcome::Violated)
                .expect("read")
                .is_none(),
            "unknown id returns None"
        );
    }

    /// Objective: Verify keyword search matches verb and object
    /// case-insensitively and scopes to the subject.
    /// Invariants: "医院" matches the promise; a keyword in another subject's
    /// decision does not leak; empty keyword returns everything.
    #[test]
    fn decision_search_matches_keywords_within_subject() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        store
            .insert_decision(&sample_decision(7, "promise", "陪用户明天去医院"))
            .expect("insert decision");
        store
            .insert_decision(&sample_decision(7, "decide", "这周末学习 Rust"))
            .expect("insert decision");
        store
            .insert_decision(&sample_decision(8, "promise", "陪用户去医院"))
            .expect("insert other-subject decision");

        let hits = store.search_decisions(7, "医院").expect("search decisions");
        assert_eq!(hits.len(), 1, "one matching decision in subject 7");
        assert_eq!(hits[0].object, "陪用户明天去医院");

        let no_hits = store
            .search_decisions(7, "不存在的关键词")
            .expect("search decisions");
        assert!(no_hits.is_empty(), "no match → empty");

        let all = store.search_decisions(7, "").expect("search decisions");
        assert_eq!(all.len(), 2, "empty keyword matches all subject decisions");
    }

    /// Objective: Verify a malformed `because` column surfaces as
    /// InvalidData rather than a panic.
    /// Invariants: bad JSON in because → StorageError::InvalidData.
    #[test]
    fn malformed_decision_because_is_invalid_data() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        {
            let conn = store.lock_conn().expect("lock fact database");
            conn.execute(
                "INSERT INTO decisions (subject, verb, object, made_at, because) VALUES (7, 'x', 'y', 1, 'not-json')",
                [],
            )
            .expect("insert malformed decision row");
        }
        let error = store
            .get_decision(1)
            .expect_err("malformed because must fail decoding");
        assert!(
            matches!(error, Error::Storage(StorageError::InvalidData(_))),
            "malformed because is InvalidData, got {error:?}"
        );
    }

    /// Objective: Verify `insert_decision` actually enforces
    /// `validate_decision` rather than persisting malformed rows — the validator
    /// previously existed but was never called from the write path.
    /// Invariants: a blank object and a blank verb are rejected with
    /// `InvalidInput` and leave no row behind; a valid decision still inserts.
    #[test]
    fn insert_decision_rejects_invalid_fields() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");

        let mut blank_object = sample_decision(7, "promise", "占位");
        blank_object.object = "   ".to_string();
        let error = store
            .insert_decision(&blank_object)
            .expect_err("a blank object must be rejected");
        assert!(
            matches!(error, Error::InvalidInput(_)),
            "a blank object is InvalidInput, got {error:?}"
        );

        let mut blank_verb = sample_decision(7, "promise", "占位");
        blank_verb.verb = String::new();
        let error = store
            .insert_decision(&blank_verb)
            .expect_err("a blank verb must be rejected");
        assert!(
            matches!(error, Error::InvalidInput(_)),
            "a blank verb is InvalidInput, got {error:?}"
        );

        let id = store
            .insert_decision(&sample_decision(7, "promise", "明天去医院"))
            .expect("a valid decision still inserts");
        assert!(id > 0, "a valid decision must be persisted");
        assert_eq!(
            store.get_decisions(7).expect("list decisions").len(),
            1,
            "rejected decisions must not leave a row behind"
        );
    }

    /// Objective: Verify the "apply the outcome exactly once" guard lives in the
    /// SQL statement, making the check and the write one atomic step. A
    /// read-then-write pair can be interleaved by two concurrent callers, and
    /// the later one silently overwrote the earlier outcome.
    /// Invariants: the first guarded update changes exactly 1 row, a second
    /// changes 0 rows, an unknown id changes 0 rows, and the stored outcome
    /// keeps the first recorded value while the status becomes closed.
    #[test]
    fn decision_outcome_guard_is_atomic_at_the_statement_level() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        let id = store
            .insert_decision(&sample_decision(7, "promise", "明天去医院"))
            .expect("insert decision");

        let conn = store.lock_conn().expect("lock fact database");
        let first = SqliteFactStore::apply_outcome_once(
            &conn,
            id,
            crate::decision::DecisionOutcome::Fulfilled,
        )
        .expect("first guarded update");
        assert_eq!(first, 1, "the first outcome must be recorded");

        let second = SqliteFactStore::apply_outcome_once(
            &conn,
            id,
            crate::decision::DecisionOutcome::Violated,
        )
        .expect("second guarded update");
        assert_eq!(
            second, 0,
            "an already-recorded outcome must never be overwritten"
        );

        let unknown = SqliteFactStore::apply_outcome_once(
            &conn,
            id + 9_999,
            crate::decision::DecisionOutcome::Violated,
        )
        .expect("guarded update on an unknown id");
        assert_eq!(unknown, 0, "an unknown decision must change nothing");
        drop(conn);

        let stored = store
            .get_decision(id)
            .expect("read decision")
            .expect("decision exists");
        assert_eq!(
            stored.outcome,
            Some(crate::decision::DecisionOutcome::Fulfilled),
            "the first recorded outcome must survive the overwrite attempt"
        );
        assert_eq!(
            stored.status,
            crate::decision::DecisionStatus::Closed,
            "recording an outcome closes the decision"
        );
    }

    /// Objective: Verify a compilation commits atomically — facts, anchors and
    /// decisions land together or not at all. The previous compile path wrote
    /// the facts first, so a rejected decision returned an error while the facts
    /// (and sometimes an orphan anchor) stayed behind for a retry to duplicate.
    /// Invariants: a rejected decision leaves zero facts and zero decisions; a
    /// valid compilation stores the utterance plus its anchor and links the
    /// decision to the anchor first, then to the supporting fact.
    #[test]
    fn compilation_is_atomic_and_links_supporting_facts() {
        use crate::commitment::{anchor_fact, commitments_from_messages};
        use crate::types::Message;

        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        let subject = store
            .resolve_user("tenant-a", "alice")
            .expect("resolve the user entity");
        let statement = "我答应你明天陪你去医院";
        let messages = vec![Message::new("user", statement)];
        let decisions = commitments_from_messages(&messages, "user", subject, 2026);
        assert_eq!(decisions.len(), 1, "the utterance is a commitment");
        let anchor = anchor_fact(&decisions[0], 2026);
        let utterance = crate::cognition::Fact {
            id: None,
            entity_id: subject,
            fact_type: crate::cognition::FactType::Event,
            time: 2026,
            payload: serde_json::json!({ "content": statement }),
            evidence_id: None,
            created_at: 2026,
            ..crate::cognition::Fact::default()
        };

        let mut invalid = decisions[0].clone();
        invalid.object = "   ".to_string();
        let error = store
            .insert_compilation(
                std::slice::from_ref(&utterance),
                &[(anchor.clone(), invalid)],
            )
            .expect_err("a blank object must abort the compilation");
        assert!(
            matches!(error, Error::InvalidInput(_)),
            "a blank object is InvalidInput, got {error:?}"
        );
        assert!(
            store.get_facts(subject).expect("read facts").is_empty(),
            "an aborted compilation must not leave facts behind"
        );
        assert!(
            store
                .get_decisions(subject)
                .expect("read decisions")
                .is_empty(),
            "an aborted compilation must not leave decisions behind"
        );

        let (facts, recorded) = store
            .insert_compilation(
                std::slice::from_ref(&utterance),
                &[(anchor, decisions[0].clone())],
            )
            .expect("commit the compilation");
        assert_eq!(facts, 2, "the utterance and its anchor are both stored");
        assert_eq!(recorded, 1, "one decision is recorded");
        let rows = store.get_facts(subject).expect("read facts");
        let utterance_id = rows[0].id.expect("the utterance has an id");
        let anchor_id = rows[1].id.expect("the anchor has an id");
        let stored = store.get_decisions(subject).expect("read decisions");
        assert_eq!(
            stored[0].because,
            vec![anchor_id, utterance_id],
            "the decision links its anchor first, then the supporting fact"
        );
    }

    /// Objective: Verify `search_decisions` escapes LIKE wildcards — a keyword
    /// of `%` must match only the decisions that literally contain `%`, not
    /// every row, and `_` must not act as a single-character wildcard.
    /// Invariants: `%`, `_` and `\` each match exactly their literal decision.
    #[test]
    fn search_decisions_escapes_like_wildcards() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        store
            .insert_decision(&sample_decision(7, "report", "进度 100% 完成"))
            .expect("insert percent decision");
        store
            .insert_decision(&sample_decision(7, "report", "文件 a_b 已归档"))
            .expect("insert underscore decision");
        store
            .insert_decision(&sample_decision(7, "report", "路径 C:\\data 已备份"))
            .expect("insert backslash decision");

        let percent = store.search_decisions(7, "%").expect("search percent");
        assert_eq!(percent.len(), 1, "a `%` keyword must match literally");
        assert_eq!(percent[0].object, "进度 100% 完成");

        let underscore = store.search_decisions(7, "_").expect("search underscore");
        assert_eq!(underscore.len(), 1, "an `_` keyword must match literally");
        assert_eq!(underscore[0].object, "文件 a_b 已归档");

        let backslash = store.search_decisions(7, "\\").expect("search backslash");
        assert_eq!(
            backslash.len(),
            1,
            "a backslash keyword must match literally"
        );
        assert_eq!(backslash[0].object, "路径 C:\\data 已备份");
    }
}
