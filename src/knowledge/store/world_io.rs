//! Inherent SQLite I/O for V7 world events and world states.
//!
//! Split out of `mod.rs` so the `KnowledgeStore` trait impl stays under the
//! 1000-line cap (plan/rules/rules.md §1): the trait methods in `mod.rs`
//! delegate here with thin wrappers.

use rusqlite::{OptionalExtension, params};

use crate::error::Result;

use super::{
    EventParticipantRef, NewWorldEvent, NewWorldState, SQLiteKnowledgeStore, WorldEntity,
    WorldEvent, WorldProfile, WorldRelation, WorldState,
};

impl SQLiteKnowledgeStore {
    /// Find a world entity by exact name.
    pub(super) async fn find_world_entity_row(&self, name: &str) -> Result<Option<WorldEntity>> {
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT id, name, entity_type, importance FROM world_entities WHERE name = ?1",
                params![name],
                |r| {
                    Ok(WorldEntity {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        entity_type: r.get(2)?,
                        importance: r.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Atomic upsert by UNIQUE(name): INSERT ... ON CONFLICT ... RETURNING id.
    pub(super) async fn upsert_world_entity_row(
        &self,
        name: &str,
        entity_type: &str,
        importance: f64,
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "INSERT INTO world_entities (name, entity_type, importance)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET
                 entity_type = excluded.entity_type,
                 importance = excluded.importance,
                 updated_at = strftime('%s','now')
             RETURNING id",
            params![name, entity_type, importance],
            |r| r.get::<_, i64>(0),
        )
        .map_err(Into::into)
    }

    /// Upsert a profile key/value; `evidence_id` is COALESCEd so a later
    /// call without evidence never clears an existing anchor.
    pub(super) async fn upsert_world_profile_row(
        &self,
        entity_id: i64,
        key: &str,
        value: &str,
        confidence: f64,
        evidence_id: Option<i64>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO world_entity_profiles (entity_id, key, value, confidence, evidence_id) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(entity_id, key) DO UPDATE SET value = excluded.value, \
                 confidence = excluded.confidence, \
                 evidence_id = COALESCE(excluded.evidence_id, world_entity_profiles.evidence_id)",
            params![entity_id, key, value, confidence, evidence_id],
        )?;
        Ok(())
    }

    /// Upsert a world relation; re-inserting the same edge updates confidence.
    pub(super) async fn upsert_world_relation_row(
        &self,
        source_id: i64,
        target_id: i64,
        relation_type: &str,
        confidence: f64,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO world_relations (source_id, target_id, relation_type, confidence) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(source_id, target_id, relation_type) DO UPDATE SET \
                 confidence = excluded.confidence",
            params![source_id, target_id, relation_type, confidence],
        )?;
        Ok(())
    }

    /// List all world entities ordered by id.
    pub(super) async fn list_world_entities_row(&self) -> Result<Vec<WorldEntity>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, name, entity_type, importance FROM world_entities ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(WorldEntity {
                id: r.get("id")?,
                name: r.get("name")?,
                entity_type: r.get("entity_type")?,
                importance: r.get("importance")?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// List all world profiles ordered by id.
    pub(super) async fn list_world_profiles_row(&self) -> Result<Vec<WorldProfile>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT entity_id, key, value, confidence, evidence_id \
             FROM world_entity_profiles ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(WorldProfile {
                entity_id: r.get("entity_id")?,
                key: r.get("key")?,
                value: r.get("value")?,
                confidence: r.get("confidence")?,
                evidence_id: r.get("evidence_id")?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// List all world relations ordered by id.
    pub(super) async fn list_world_relations_row(&self) -> Result<Vec<WorldRelation>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT source_id, target_id, relation_type, confidence \
             FROM world_relations ORDER BY rowid ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(WorldRelation {
                source_id: r.get("source_id")?,
                target_id: r.get("target_id")?,
                relation_type: r.get("relation_type")?,
                confidence: r.get("confidence")?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Insert a world event, or return the existing id when an event with the
    /// same `(title, timestamp, start_offset, end_offset)` already exists.
    ///
    /// The offset pair is part of the identity so the same title at two
    /// different source spans stays two events, while a re-compile of the
    /// same sentence is a no-op. The unique expression index (created in
    /// `init` after `ensure_column`) makes the check-then-insert a single
    /// atomic statement — two connections compiling the same document can no
    /// longer double-insert (same P2 fix as `world_entities`).
    pub(super) async fn upsert_world_event_row(&self, event: NewWorldEvent<'_>) -> Result<i64> {
        let conn = self.conn.lock().await;
        // DO UPDATE (not DO NOTHING) so RETURNING always yields a row id;
        // non-identity columns are refreshed from the latest compile.
        conn.query_row(
            "INSERT INTO events \
             (title, event_type, timestamp, location, description, importance, \
              start_offset, end_offset) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT(title, IFNULL(timestamp, -1), IFNULL(start_offset, -1), \
                         IFNULL(end_offset, -1)) \
             DO UPDATE SET \
                 event_type = excluded.event_type, \
                 location = excluded.location, \
                 description = excluded.description, \
                 importance = excluded.importance \
             RETURNING id",
            params![
                event.title,
                event.event_type,
                event.timestamp,
                event.location,
                event.description,
                event.importance,
                event.start_offset,
                event.end_offset
            ],
            |r| r.get::<_, i64>(0),
        )
        .map_err(Into::into)
    }

    /// Link an event to a world entity by name (upserting the entity when it
    /// is not yet in `world_entities`). Idempotent via the
    /// `event_participants UNIQUE(event_id, entity_id)` constraint.
    ///
    /// The entity upsert uses `RETURNING id` (not `last_insert_rowid`): on the
    /// `ON CONFLICT DO UPDATE` path `last_insert_rowid` is NOT updated — it
    /// returns whatever row this connection inserted last, which under a
    /// cross-store race would link the participant to the WRONG entity.
    pub(super) async fn link_event_participant_row(
        &self,
        event_id: i64,
        entity_name: &str,
        role: &str,
    ) -> Result<()> {
        let entity_id: i64 = {
            let conn = self.conn.lock().await;
            conn.query_row(
                "INSERT INTO world_entities (name, entity_type, importance) \
                 VALUES (?1, 'person', 0.5) \
                 ON CONFLICT(name) DO UPDATE SET updated_at = strftime('%s','localtime') \
                 RETURNING id",
                params![entity_name],
                |r| r.get(0),
            )?
        };
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO event_participants (event_id, entity_id, role) \
             VALUES (?1, ?2, ?3)",
            params![event_id, entity_id, role],
        )?;
        Ok(())
    }

    /// List all world events ordered by id.
    pub(super) async fn list_world_events_row(&self) -> Result<Vec<WorldEvent>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, title, event_type, timestamp, location, description, \
                    importance, start_offset, end_offset \
             FROM events ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(WorldEvent {
                id: r.get("id")?,
                title: r.get("title")?,
                event_type: r.get("event_type")?,
                timestamp: r.get("timestamp")?,
                location: r.get("location")?,
                description: r.get("description")?,
                importance: r.get("importance")?,
                start_offset: r.get("start_offset")?,
                end_offset: r.get("end_offset")?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Resolve one event's participants to (entity name, role).
    ///
    /// The JOIN converts the local `entity_id` FK into the portable name so
    /// export can carry participants across databases.
    pub(super) async fn list_event_participants_row(
        &self,
        event_id: i64,
    ) -> Result<Vec<EventParticipantRef>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT we.name, ep.role \
             FROM event_participants ep \
             JOIN world_entities we ON we.id = ep.entity_id \
             WHERE ep.event_id = ?1 \
             ORDER BY ep.id ASC",
        )?;
        let rows = stmt.query_map(params![event_id], |r| {
            Ok(EventParticipantRef {
                entity_name: r.get(0)?,
                role: r.get(1)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Upsert a character-state slot anchored to its source event.
    ///
    /// Identity is `(entity_id, slot, event_id, chapter)`: re-compiling the same
    /// document reuses the event id (see `upsert_world_event_row`) so the
    /// state row is a no-op, while the same slot from a *different* event
    /// appends a new history row (ADD-only — state is reconstructable).
    /// Enforced atomically by the unique expression index (see `init`).
    ///
    /// Returns the `world_states.id`.
    pub(super) async fn upsert_world_state_row(&self, state: NewWorldState<'_>) -> Result<i64> {
        let NewWorldState {
            entity_name,
            slot,
            value,
            chapter,
            event_id,
            start_offset,
            end_offset,
            confidence,
        } = state;
        // Participants must exist in world_states' FK target (world_entities).
        // RETURNING id — never `last_insert_rowid`, which is stale on the
        // ON CONFLICT DO UPDATE path (see link_event_participant_row).
        let entity_id: i64 = {
            let conn = self.conn.lock().await;
            conn.query_row(
                "INSERT INTO world_entities (name, entity_type, importance) \
                 VALUES (?1, 'person', 0.5) \
                 ON CONFLICT(name) DO UPDATE SET updated_at = strftime('%s','localtime') \
                 RETURNING id",
                params![entity_name],
                |r| r.get(0),
            )?
        };

        let conn = self.conn.lock().await;
        // DO UPDATE so RETURNING always yields a row id; identical identity
        // refreshes value/confidence and keeps the wider of the two spans.
        conn.query_row(
            "INSERT INTO world_states \
             (entity_id, slot, value, chapter, event_id, start_offset, end_offset, confidence) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT(entity_id, slot, IFNULL(event_id, -1), IFNULL(chapter, -1)) \
             DO UPDATE SET \
                 value = excluded.value, \
                 confidence = excluded.confidence, \
                 start_offset = COALESCE(excluded.start_offset, world_states.start_offset), \
                 end_offset = COALESCE(excluded.end_offset, world_states.end_offset) \
             RETURNING id",
            params![
                entity_id,
                slot,
                value,
                chapter,
                event_id,
                start_offset,
                end_offset,
                confidence
            ],
            |r| r.get::<_, i64>(0),
        )
        .map_err(Into::into)
    }

    /// List world states, optionally filtered by entity name, ordered by
    /// (chapter, id) so the history reads chronologically.
    pub(super) async fn list_world_states_row(
        &self,
        entity_name: Option<&str>,
    ) -> Result<Vec<WorldState>> {
        let conn = self.conn.lock().await;
        let sql = if entity_name.is_some() {
            "SELECT s.id, e.name, s.slot, s.value, s.chapter, s.event_id, \
                    s.start_offset, s.end_offset, s.confidence \
             FROM world_states s \
             JOIN world_entities e ON e.id = s.entity_id \
             WHERE e.name = ?1 \
             ORDER BY s.chapter ASC, s.id ASC"
        } else {
            "SELECT s.id, e.name, s.slot, s.value, s.chapter, s.event_id, \
                    s.start_offset, s.end_offset, s.confidence \
             FROM world_states s \
             JOIN world_entities e ON e.id = s.entity_id \
             ORDER BY s.chapter ASC, s.id ASC"
        };
        let map_row = |r: &rusqlite::Row| {
            Ok(WorldState {
                id: r.get("id")?,
                entity_name: r.get("name")?,
                slot: r.get("slot")?,
                value: r.get("value")?,
                chapter: r.get("chapter")?,
                event_id: r.get("event_id")?,
                start_offset: r.get("start_offset")?,
                end_offset: r.get("end_offset")?,
                confidence: r.get("confidence")?,
            })
        };
        let mut out = Vec::new();
        match entity_name {
            Some(name) => {
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt.query_map(params![name], map_row)?;
                for r in rows {
                    out.push(r?);
                }
            }
            None => {
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt.query_map([], map_row)?;
                for r in rows {
                    out.push(r?);
                }
            }
        }
        Ok(out)
    }
}
