//! World-model export sections: narrative events (with participants) and
//! character-state slots — the T6/T7 write paths made these durable, so a
//! backup that dropped them would lose the story timeline and status history.
//!
//! Numeric ids (`events.id`, `event_participants.*`, `world_states.event_id`)
//! are local to each database. Events are portable by their UPSERT IDENTITY
//! (title + byte span); a state's event anchor travels as that identity and
//! is re-pointed to the importing store's event id. Roles travel as names
//! (`world_entities` rows are upserted by name on import).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::knowledge::store::{EventParticipantRef, KnowledgeStore, NewWorldEvent, NewWorldState};

/// One participant of an exported event: entity name + role.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExportEventParticipant {
    /// World-entity name (upserted by name on import).
    pub entity_name: String,
    /// Role (`subject` / `object` / `speaker` / `participant` / …).
    pub role: String,
}

/// A narrative `events` row in portable form: identity + payload + the
/// participants resolved to names (no local ids).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportWorldEvent {
    /// Event title — part of the upsert identity with the byte span.
    pub title: String,
    /// Free-form type (`action` / `dialogue` / …).
    pub event_type: String,
    /// Narrative time (chapter or year), when known.
    pub timestamp: Option<i32>,
    /// Where it happened, when known.
    pub location: Option<String>,
    /// One-line description — usually the source sentence.
    pub description: String,
    /// Importance in `[0, 1]`.
    pub importance: f64,
    /// Source byte span start (part of the identity).
    pub start_offset: Option<i64>,
    /// Source byte span end (part of the identity).
    pub end_offset: Option<i64>,
    /// Who took part, by entity name (ordered by row id).
    #[serde(default)]
    pub participants: Vec<ExportEventParticipant>,
}

/// An event reference by UPSERT IDENTITY (title + timestamp + byte span) —
/// the only portable key, because `events.id` differs between databases.
/// Mirrors the `ux_events_identity` key exactly: a title at two timestamps
/// is two events, so the anchor must carry the timestamp too or it could
/// re-point at the wrong row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ExportEventIdentity {
    /// Event title.
    pub title: String,
    /// Narrative time (part of the upsert identity).
    pub timestamp: Option<i32>,
    /// Source byte span start.
    pub start_offset: Option<i64>,
    /// Source byte span end.
    pub end_offset: Option<i64>,
}

/// A `world_states` observation in portable form; the event anchor is an
/// [`ExportEventIdentity`] so import can re-point `event_id` locally.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportWorldState {
    /// Entity whose slot this is (upserted into `world_entities` by name).
    pub entity_name: String,
    /// Slot name (`status` / `location` / …).
    pub slot: String,
    /// Observed value (`deceased` / …).
    pub value: String,
    /// Narrative chapter, when known.
    pub chapter: Option<i32>,
    /// Event that produced this observation, by identity; `None` for a
    /// manual write.
    #[serde(default)]
    pub event: Option<ExportEventIdentity>,
    /// Source byte span start of the observation.
    pub start_offset: Option<i64>,
    /// Source byte span end of the observation.
    pub end_offset: Option<i64>,
    /// Confidence in `[0, 1]`.
    pub confidence: f64,
}

/// Export every event (with its participants) and every world state.
///
/// States keep their event anchor as an identity (title + span); a state
/// whose anchor row is somehow missing exports with `event: None` instead
/// of a dangling numeric id.
pub(crate) async fn export_world_sections(
    store: &dyn KnowledgeStore,
) -> Result<(Vec<ExportWorldEvent>, Vec<ExportWorldState>)> {
    let events = store.list_world_events().await?;
    let mut identity_by_id: HashMap<i64, ExportEventIdentity> = HashMap::new();
    let mut out_events = Vec::with_capacity(events.len());
    for e in &events {
        identity_by_id.insert(
            e.id,
            ExportEventIdentity {
                title: e.title.clone(),
                timestamp: e.timestamp,
                start_offset: e.start_offset,
                end_offset: e.end_offset,
            },
        );
        let participants = store
            .list_event_participants(e.id)
            .await?
            .into_iter()
            .map(
                |EventParticipantRef { entity_name, role }| ExportEventParticipant {
                    entity_name,
                    role,
                },
            )
            .collect();
        out_events.push(ExportWorldEvent {
            title: e.title.clone(),
            event_type: e.event_type.clone(),
            timestamp: e.timestamp,
            location: e.location.clone(),
            description: e.description.clone(),
            importance: e.importance,
            start_offset: e.start_offset,
            end_offset: e.end_offset,
            participants,
        });
    }

    let out_states = store
        .list_world_states(None)
        .await?
        .into_iter()
        .map(|s| ExportWorldState {
            entity_name: s.entity_name,
            slot: s.slot,
            value: s.value,
            chapter: s.chapter,
            event: s.event_id.and_then(|id| identity_by_id.get(&id).cloned()),
            start_offset: s.start_offset,
            end_offset: s.end_offset,
            confidence: s.confidence,
        })
        .collect();
    Ok((out_events, out_states))
}

/// Import events (upserting by identity), their participants, then states.
///
/// Events run first so every state anchor can be re-pointed to THIS store's
/// event id; `link_event_participant` upserts missing `world_entities` rows
/// by name, and `upsert_world_state` does the same for state entities. A
/// state claiming an anchor that exists in neither the bundle nor the store
/// is skipped rather than imported under the wrong (NULL) identity.
///
/// Both passes are identity-idempotent: a re-import is a no-op.
pub(crate) async fn import_world_sections(
    store: &dyn KnowledgeStore,
    events: &[ExportWorldEvent],
    states: &[ExportWorldState],
) -> Result<()> {
    for e in events {
        let event_id = store
            .upsert_world_event(NewWorldEvent {
                title: &e.title,
                event_type: &e.event_type,
                timestamp: e.timestamp,
                location: e.location.as_deref(),
                description: &e.description,
                importance: e.importance,
                start_offset: e.start_offset,
                end_offset: e.end_offset,
            })
            .await?;
        for p in &e.participants {
            store
                .link_event_participant(event_id, &p.entity_name, &p.role)
                .await?;
        }
    }

    // Identity → local id for the anchor re-point: bundle events (just
    // upserted) plus anything already in the store covers partial imports.
    let mut identity_to_id: HashMap<ExportEventIdentity, i64> = HashMap::new();
    for e in store.list_world_events().await? {
        identity_to_id.insert(
            ExportEventIdentity {
                title: e.title,
                timestamp: e.timestamp,
                start_offset: e.start_offset,
                end_offset: e.end_offset,
            },
            e.id,
        );
    }

    for s in states {
        let event_id = match &s.event {
            Some(identity) => match identity_to_id.get(identity) {
                Some(&id) => Some(id),
                None => continue,
            },
            None => None,
        };
        store
            .upsert_world_state(NewWorldState {
                entity_name: &s.entity_name,
                slot: &s.slot,
                value: &s.value,
                chapter: s.chapter,
                event_id,
                start_offset: s.start_offset,
                end_offset: s.end_offset,
                confidence: s.confidence,
            })
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::memory_export::{EXPORT_FORMAT, export_store, import_bundle};
    use crate::knowledge::store::SQLiteKnowledgeStore;

    /// Seed one kill event with two participants plus one anchored state and
    /// one manual (anchor-less) state.
    async fn seed(src: &SQLiteKnowledgeStore) -> i64 {
        let event_id = src
            .upsert_world_event(NewWorldEvent {
                title: "吕布 杀 董卓",
                event_type: "action",
                timestamp: Some(3),
                location: None,
                description: "吕布杀董卓。",
                importance: 0.6,
                start_offset: Some(5),
                end_offset: Some(8),
            })
            .await
            .expect("upsert event");
        src.link_event_participant(event_id, "吕布", "subject")
            .await
            .expect("link subject");
        src.link_event_participant(event_id, "董卓", "object")
            .await
            .expect("link object");
        src.upsert_world_state(NewWorldState {
            entity_name: "董卓",
            slot: "status",
            value: "deceased",
            chapter: Some(3),
            event_id: Some(event_id),
            start_offset: Some(5),
            end_offset: Some(8),
            confidence: 0.75,
        })
        .await
        .expect("upsert anchored state");
        src.upsert_world_state(NewWorldState {
            entity_name: "吕布",
            slot: "location",
            value: "虎牢关",
            chapter: None,
            event_id: None,
            start_offset: None,
            end_offset: None,
            confidence: 0.6,
        })
        .await
        .expect("upsert manual state");
        event_id
    }

    /// Objective: Verify events + participants + states survive a full
    /// export→import round trip, with the state's event anchor re-pointed to
    /// the DESTINATION store's event id (never a copied numeric id).
    /// Invariants: 1 event (identity + payload), 2 participants with roles,
    /// 2 states, anchored state references the imported event.
    #[tokio::test]
    async fn world_events_and_states_round_trip() {
        let src = SQLiteKnowledgeStore::open_in_memory().await.expect("src");
        seed(&src).await;

        let bundle = export_store(&src).await.expect("export");
        assert_eq!(bundle.world_events.len(), 1, "one event exported");
        assert_eq!(bundle.world_states.len(), 2, "both states exported");

        let dst = SQLiteKnowledgeStore::open_in_memory().await.expect("dst");
        import_bundle(&dst, &bundle).await.expect("import");

        let events = dst.list_world_events().await.expect("list events");
        assert_eq!(events.len(), 1, "one event imported");
        let ev = &events[0];
        assert_eq!(ev.title, "吕布 杀 董卓");
        assert_eq!(ev.event_type, "action");
        assert_eq!(ev.timestamp, Some(3));
        assert_eq!((ev.start_offset, ev.end_offset), (Some(5), Some(8)));

        let parts = dst
            .list_event_participants(ev.id)
            .await
            .expect("participants");
        let named: Vec<(&str, &str)> = parts
            .iter()
            .map(|p| (p.entity_name.as_str(), p.role.as_str()))
            .collect();
        assert_eq!(
            named,
            vec![("吕布", "subject"), ("董卓", "object")],
            "participants keep name+role order"
        );

        let states = dst.list_world_states(None).await.expect("states");
        assert_eq!(states.len(), 2, "both states imported");
        let victim = states
            .iter()
            .find(|s| s.slot == "status")
            .expect("anchored status state");
        assert_eq!(victim.entity_name, "董卓");
        assert_eq!(victim.value, "deceased");
        assert_eq!(
            victim.event_id,
            Some(ev.id),
            "anchor must point at the DESTINATION event id"
        );
        assert!((victim.confidence - 0.75).abs() < f64::EPSILON);
        let manual = states
            .iter()
            .find(|s| s.slot == "location")
            .expect("manual state");
        assert_eq!(
            manual.event_id, None,
            "a state without an anchor keeps event_id NULL"
        );
    }

    /// Objective: Verify re-importing the same bundle is a no-op (identity
    /// upserts + UNIQUE(event_id, entity_id) participants).
    /// Invariants: counts stay 1 event / 2 participants / 2 states.
    #[tokio::test]
    async fn reimport_is_idempotent() {
        let src = SQLiteKnowledgeStore::open_in_memory().await.expect("src");
        seed(&src).await;
        let bundle = export_store(&src).await.expect("export");

        let dst = SQLiteKnowledgeStore::open_in_memory().await.expect("dst");
        import_bundle(&dst, &bundle).await.expect("first import");
        import_bundle(&dst, &bundle).await.expect("second import");

        let events = dst.list_world_events().await.expect("events");
        assert_eq!(events.len(), 1, "event must not duplicate");
        let parts = dst
            .list_event_participants(events[0].id)
            .await
            .expect("parts");
        assert_eq!(parts.len(), 2, "participants must not duplicate");
        let states = dst.list_world_states(None).await.expect("states");
        assert_eq!(states.len(), 2, "states must not duplicate");
    }

    /// Objective: Verify the state event-anchor key includes TIMESTAMP:
    /// two events sharing title+span but differing in timestamp are distinct
    /// rows (ux_events_identity), so a span-only key could re-point a state
    /// at the wrong event after import.
    /// Invariants: both events survive; the state resolves to the event with
    /// the anchored timestamp, not just any same-title-same-span row.
    #[tokio::test]
    async fn state_anchor_distinguishes_same_title_span_by_timestamp() {
        let src = SQLiteKnowledgeStore::open_in_memory().await.expect("src");
        let mk = |timestamp: i32| NewWorldEvent {
            title: "同一句",
            event_type: "action",
            timestamp: Some(timestamp),
            location: None,
            description: "同一句原文",
            importance: 0.5,
            start_offset: Some(0),
            end_offset: Some(4),
        };
        let ea = src.upsert_world_event(mk(3)).await.expect("event a");
        let eb = src.upsert_world_event(mk(7)).await.expect("event b");
        assert_ne!(ea, eb, "same title+span, different timestamp → two rows");
        src.upsert_world_state(NewWorldState {
            entity_name: "甲",
            slot: "status",
            value: "deceased",
            chapter: Some(3),
            event_id: Some(ea),
            start_offset: Some(0),
            end_offset: Some(4),
            confidence: 0.7,
        })
        .await
        .expect("state");

        let bundle = export_store(&src).await.expect("export");
        assert_eq!(
            bundle.world_states[0].event.as_ref().map(|e| e.timestamp),
            Some(Some(3)),
            "the exported anchor must carry the timestamp"
        );

        let dst = SQLiteKnowledgeStore::open_in_memory().await.expect("dst");
        import_bundle(&dst, &bundle).await.expect("import");
        let events = dst.list_world_events().await.expect("events");
        assert_eq!(events.len(), 2, "both timestamp rows survive");
        let states = dst.list_world_states(None).await.expect("states");
        let anchored_id = states[0].event_id.expect("state keeps its anchor");
        let target = events
            .iter()
            .find(|e| e.id == anchored_id)
            .expect("anchor resolves");
        assert_eq!(
            target.timestamp,
            Some(3),
            "anchor must resolve to the timestamped row, not any same-span row"
        );
    }

    /// Objective: Verify a v1 bundle (written before these sections existed)
    /// still deserializes — the new fields carry `#[serde(default)]`.
    /// Invariants: missing world_events/world_states keys → empty vecs;
    /// importing it into a store succeeds.
    #[tokio::test]
    async fn v1_bundle_without_world_sections_imports() {
        let json = serde_json::json!({
            "format": EXPORT_FORMAT,
            "version": 1,
            "exported_at": 0,
            "documents": [],
            "objects": [],
            "edges": [],
            "evidence": [],
            "evidence_links": [],
        });
        let bundle: crate::knowledge::memory_export::ExportBundle =
            serde_json::from_value(json).expect("v1 bundle must deserialize");
        assert!(bundle.world_events.is_empty());
        assert!(bundle.world_states.is_empty());

        let dst = SQLiteKnowledgeStore::open_in_memory().await.expect("dst");
        import_bundle(&dst, &bundle)
            .await
            .expect("v1 bundle imports cleanly");
        assert_eq!(
            dst.list_world_events().await.expect("events").len(),
            0,
            "nothing to import"
        );
    }
}
