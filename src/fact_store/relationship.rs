//! Relationship-state persistence: the agent/user relationship snapshot.

use std::str::FromStr;

use super::*;
use crate::relationship::{EmotionTrend, RelationshipStage, RelationshipState};

impl SqliteFactStore {
    /// Load a relationship state row by its identity triple.
    ///
    /// Returns `Ok(None)` when no relationship has been recorded yet.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the row cannot be read or decoded.
    pub(crate) fn load_relationship(
        &self,
        tenant_id: &str,
        agent_entity_id: i64,
        user_entity_id: i64,
    ) -> Result<Option<RelationshipState>> {
        let conn = self.lock_conn()?;
        load_relationship_unlocked(&conn, tenant_id, agent_entity_id, user_entity_id)
    }

    /// Atomically read-modify-write a relationship inside ONE lock critical
    /// section.
    ///
    /// `apply_messages` used to call `load_relationship` and
    /// `save_relationship` as two separate lock acquisitions: two concurrent
    /// calls for the same (agent, user) pair both read the old intimacy, each
    /// added its own delta, and one increment was silently lost. Holding the
    /// connection lock across load → compute → save serializes the pair, so
    /// every delta is preserved.
    ///
    /// `compute` receives the current state (None when the pair has no
    /// relationship yet) and returns the new state to persist.
    pub(crate) fn update_relationship_atomic(
        &self,
        tenant_id: &str,
        agent_entity_id: i64,
        user_entity_id: i64,
        compute: impl FnOnce(Option<&RelationshipState>) -> RelationshipState,
    ) -> Result<RelationshipState> {
        let conn = self.lock_conn()?;
        let current =
            load_relationship_unlocked(&conn, tenant_id, agent_entity_id, user_entity_id)?;
        let updated = compute(current.as_ref());
        save_relationship_unlocked(&conn, &updated)?;
        Ok(updated)
    }
}

/// Upsert a relationship row on an already-locked connection (no lock
/// acquisition — callers hold it).
///
/// Returns the row's stable id via `RETURNING id`. A plain
/// `last_insert_rowid()` would be STALE on the conflict path: SQLite does
/// not update it for `ON CONFLICT DO UPDATE`, so any INSERT between two
/// saves of the same pair would hand back the interloper's id (the same
/// defect class as the T8 `world_io` Fix#3 audit).
fn save_relationship_unlocked(conn: &Connection, rs: &RelationshipState) -> Result<i64> {
    let id: i64 = conn.query_row(
        "INSERT INTO relationship_state
             (tenant_id, agent_entity_id, user_entity_id, intimacy, stage, emotion_trend, recent_topics, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(tenant_id, agent_entity_id, user_entity_id) DO UPDATE SET
             intimacy      = excluded.intimacy,
             stage         = excluded.stage,
             emotion_trend = excluded.emotion_trend,
             recent_topics = excluded.recent_topics,
             updated_at    = excluded.updated_at
         RETURNING id",
        params![
            rs.tenant_id,
            rs.agent_entity_id,
            rs.user_entity_id,
            rs.intimacy,
            rs.stage.as_str(),
            rs.emotion_trend.as_str(),
            serde_json::to_string(&rs.recent_topics)?,
            rs.updated_at,
        ],
        |row| row.get(0),
    )?;
    Ok(id)
}

/// Load a relationship row on an already-locked connection.
///
/// Returns `Ok(None)` when no relationship has been recorded yet.
fn load_relationship_unlocked(
    conn: &Connection,
    tenant_id: &str,
    agent_entity_id: i64,
    user_entity_id: i64,
) -> Result<Option<RelationshipState>> {
    let row = conn
        .query_row(
            "SELECT tenant_id, agent_entity_id, user_entity_id, intimacy, stage, emotion_trend, recent_topics, updated_at
             FROM relationship_state
             WHERE tenant_id = ?1 AND agent_entity_id = ?2 AND user_entity_id = ?3",
            params![tenant_id, agent_entity_id, user_entity_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((
        tenant_id,
        agent_entity_id,
        user_entity_id,
        intimacy,
        stage,
        trend,
        topics_json,
        updated_at,
    )) = row
    else {
        return Ok(None);
    };
    let recent_topics: Vec<String> = serde_json::from_str(&topics_json).map_err(|error| {
        Error::Storage(StorageError::InvalidData(format!(
            "relationship recent_topics is not valid JSON: {error}"
        )))
    })?;
    let stage = RelationshipStage::from_str(&stage).map_err(|message| {
        Error::Storage(StorageError::InvalidData(format!(
            "relationship stage `{stage}` is invalid: {message}"
        )))
    })?;
    let emotion_trend = EmotionTrend::from_str(&trend).map_err(|message| {
        Error::Storage(StorageError::InvalidData(format!(
            "relationship emotion_trend `{trend}` is invalid: {message}"
        )))
    })?;
    Ok(Some(RelationshipState {
        tenant_id,
        agent_entity_id,
        user_entity_id,
        intimacy,
        stage,
        emotion_trend,
        recent_topics,
        updated_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(tenant: &str, agent: i64, user: i64, intimacy: f64) -> RelationshipState {
        RelationshipState {
            tenant_id: tenant.into(),
            agent_entity_id: agent,
            user_entity_id: user,
            intimacy,
            stage: RelationshipStage::Stranger,
            emotion_trend: EmotionTrend::Stable,
            recent_topics: Vec::new(),
            updated_at: 0,
        }
    }

    /// Objective: Verify an upsert returns the row's id on BOTH paths.
    /// `ON CONFLICT DO UPDATE` does not refresh `last_insert_rowid()`, so
    /// an interloper INSERT between two saves of the same pair used to leak
    /// its id (T8 last_insert_rowid audit — the relationship.rs hit).
    /// Invariants: first save → new id; distinct pair → distinct id;
    /// re-saving the first pair returns the FIRST id, not the interloper's.
    #[test]
    fn save_relationship_returns_stable_id_on_updates() {
        let store = SqliteFactStore::open_in_memory().expect("open fact store");
        let conn = store.lock_conn().expect("lock conn");

        let first = state("t", 1, 2, 0.1);
        let id1 = save_relationship_unlocked(&conn, &first).expect("insert");

        let interloper = state("t", 1, 3, 0.2);
        let id2 = save_relationship_unlocked(&conn, &interloper).expect("insert");
        assert_ne!(id1, id2, "distinct identity triples get distinct rows");

        let updated = state("t", 1, 2, 0.9);
        let id3 = save_relationship_unlocked(&conn, &updated).expect("update");
        assert_eq!(
            id3, id1,
            "the conflict path must return the EXISTING row id, not the last insert's"
        );
    }
}
