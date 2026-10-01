//! Knowledge-store unit tests, split by area so no file crosses the
//! one-file-per-1000-lines rule (`plan/rules/rules.md` §1). The shared helpers
//! (`fresh`, `seed_doc`, …) stay in the parent `tests` module and are reached
//! through `use super::*`.

use super::*;

/// Objective: Verify world events persist their source byte spans and that
/// re-upserting the same (title, timestamp, span) is idempotent while the
/// same title at a different span stays a separate event.
/// Invariants: offsets round-trip; second upsert returns the same id; a
/// different start_offset creates a new row; participant links are no-ops
/// on repeat.
#[tokio::test]
async fn world_event_offsets_round_trip_and_upsert_is_idempotent() {
    let store = fresh().await;
    let id1 = store
        .upsert_world_event(NewWorldEvent {
            title: "刘备曰",
            event_type: "dialogue",
            timestamp: Some(3),
            description: "刘备曰：进攻",
            importance: 0.5,
            start_offset: Some(100),
            end_offset: Some(120),
            ..NewWorldEvent::default()
        })
        .await
        .expect("insert event");
    // Same identity → same id (re-compile must not duplicate).
    let id1_again = store
        .upsert_world_event(NewWorldEvent {
            title: "刘备曰",
            event_type: "dialogue",
            timestamp: Some(3),
            description: "刘备曰：进攻",
            importance: 0.5,
            start_offset: Some(100),
            end_offset: Some(120),
            ..NewWorldEvent::default()
        })
        .await
        .expect("re-insert");
    assert_eq!(id1, id1_again, "identical (title, ts, span) reuses the row");

    // Same title at a different span is a distinct event.
    let id2 = store
        .upsert_world_event(NewWorldEvent {
            title: "刘备曰",
            event_type: "dialogue",
            timestamp: Some(3),
            description: "刘备曰：撤退",
            importance: 0.5,
            start_offset: Some(500),
            end_offset: Some(520),
            ..NewWorldEvent::default()
        })
        .await
        .expect("insert second");
    assert_ne!(id1, id2, "different span must not merge events");

    let events = store.list_world_events().await.expect("list");
    assert_eq!(events.len(), 2, "two span-distinct events");
    let first = events.iter().find(|e| e.id == id1).expect("first event");
    assert_eq!(first.start_offset, Some(100), "start span persisted");
    assert_eq!(first.end_offset, Some(120), "end span persisted");
    assert_eq!(first.timestamp, Some(3), "chapter persisted");

    // Participant link is idempotent (UNIQUE event_id+entity_id).
    store
        .link_event_participant(id1, "刘备", "speaker")
        .await
        .expect("link once");
    store
        .link_event_participant(id1, "刘备", "speaker")
        .await
        .expect("link twice is a no-op");
    let world = store
        .find_world_entity("刘备")
        .await
        .expect("query")
        .expect("participant upserted into world_entities");
    assert_eq!(
        world.name, "刘备",
        "the participant must be upserted as a world entity"
    );
}

/// Objective: Verify world-state slots persist with their event anchor and
/// byte span, that a re-observation of the same (entity, slot, event) is
/// idempotent, and that a later chapter appends history (ADD-only).
/// Invariants: offsets round-trip; second upsert returns the same id and
/// does not grow the list; a different chapter adds a row; entity filter
/// scopes the result.
#[tokio::test]
async fn world_state_slots_round_trip_and_append_history() {
    let store = fresh().await;
    let event_id = store
        .upsert_world_event(NewWorldEvent {
            title: "吕布 杀 董卓",
            event_type: "action",
            timestamp: Some(3),
            description: "吕布杀董卓",
            importance: 0.6,
            start_offset: Some(100),
            end_offset: Some(112),
            ..NewWorldEvent::default()
        })
        .await
        .expect("event");

    let id1 = store
        .upsert_world_state(NewWorldState {
            entity_name: "董卓",
            slot: "status",
            value: "deceased",
            chapter: Some(3),
            event_id: Some(event_id),
            start_offset: Some(100),
            end_offset: Some(112),
            confidence: 0.75,
        })
        .await
        .expect("state 1");
    // Same (entity, slot, event, chapter) → same row (re-compile no-op).
    let id1_again = store
        .upsert_world_state(NewWorldState {
            entity_name: "董卓",
            slot: "status",
            value: "deceased",
            chapter: Some(3),
            event_id: Some(event_id),
            start_offset: Some(100),
            end_offset: Some(112),
            confidence: 0.75,
        })
        .await
        .expect("state re-upsert");
    assert_eq!(id1, id1_again, "identical observation must reuse the row");

    let all = store.list_world_states(None).await.expect("list all");
    assert_eq!(all.len(), 1, "idempotent upsert must not grow history");
    assert_eq!(all[0].entity_name, "董卓", "the state must name its entity");
    assert_eq!(all[0].slot, "status", "the state must keep its slot");
    assert_eq!(all[0].value, "deceased", "the state must keep its value");
    assert_eq!(all[0].chapter, Some(3), "the state must keep its chapter");
    assert_eq!(
        all[0].event_id,
        Some(event_id),
        "state anchors to its event"
    );
    assert_eq!(all[0].start_offset, Some(100), "span persisted");
    assert_eq!(
        all[0].end_offset,
        Some(112),
        "the state must keep its end offset"
    );

    // A later chapter (different event) appends history — ADD-only.
    let event2 = store
        .upsert_world_event(NewWorldEvent {
            title: "华雄 斩 某",
            event_type: "action",
            timestamp: Some(5),
            description: "华雄斩某",
            importance: 0.6,
            start_offset: Some(200),
            end_offset: Some(210),
            ..NewWorldEvent::default()
        })
        .await
        .expect("event 2");
    store
        .upsert_world_state(NewWorldState {
            entity_name: "华雄",
            slot: "status",
            value: "deceased",
            chapter: Some(5),
            event_id: Some(event2),
            start_offset: Some(200),
            end_offset: Some(210),
            confidence: 0.7,
        })
        .await
        .expect("state 2");

    let all = store.list_world_states(None).await.expect("list all");
    assert_eq!(all.len(), 2, "a new event appends, never overwrites");
    // Ordered by (chapter, id): ch3 first, ch5 second.
    assert_eq!(
        all[0].chapter,
        Some(3),
        "the earlier chapter must sort first"
    );
    assert_eq!(
        all[1].chapter,
        Some(5),
        "the later chapter must sort second"
    );

    // Entity-name filter scopes the query.
    let one = store.list_world_states(Some("董卓")).await.expect("filter");
    assert_eq!(one.len(), 1, "filter by entity name");
    assert_eq!(
        one[0].entity_name, "董卓",
        "the entity filter must scope the result"
    );
}

/// Objective: Pin the re-observation contract for world-state spans: the same
/// (entity, slot, event, chapter) identity refreshes `value`/`confidence` and
/// replaces a span only when the new observation supplies one.
/// Invariants: a re-observation that omits both offsets keeps the stored span; a
/// re-observation that supplies a span replaces it; no recorded offset is erased.
#[tokio::test]
async fn world_state_reobservation_replaces_span_only_when_supplied() {
    let store = fresh().await;
    let event_id = store
        .upsert_world_event(NewWorldEvent {
            title: "吕布 杀 董卓",
            event_type: "action",
            timestamp: Some(3),
            description: "吕布杀董卓",
            importance: 0.6,
            start_offset: Some(100),
            end_offset: Some(112),
            ..NewWorldEvent::default()
        })
        .await
        .expect("event");

    store
        .upsert_world_state(NewWorldState {
            entity_name: "董卓",
            slot: "status",
            value: "deceased",
            chapter: Some(3),
            event_id: Some(event_id),
            start_offset: Some(100),
            end_offset: Some(112),
            confidence: 0.75,
        })
        .await
        .expect("initial state");

    // A re-observation that omits the span must not erase the stored one.
    store
        .upsert_world_state(NewWorldState {
            entity_name: "董卓",
            slot: "status",
            value: "dead",
            chapter: Some(3),
            event_id: Some(event_id),
            start_offset: None,
            end_offset: None,
            confidence: 0.9,
        })
        .await
        .expect("re-observation without a span");

    let states = store.list_world_states(Some("董卓")).await.expect("list");
    assert_eq!(states.len(), 1, "the identity must reuse the same row");
    assert_eq!(states[0].value, "dead", "value must be refreshed");
    assert_eq!(states[0].confidence, 0.9, "confidence must be refreshed");
    assert_eq!(
        states[0].start_offset,
        Some(100),
        "an omitted start offset must not erase the stored one"
    );
    assert_eq!(
        states[0].end_offset,
        Some(112),
        "an omitted end offset must not erase the stored one"
    );

    // A re-observation that supplies a span replaces the stored one.
    store
        .upsert_world_state(NewWorldState {
            entity_name: "董卓",
            slot: "status",
            value: "dead",
            chapter: Some(3),
            event_id: Some(event_id),
            start_offset: Some(200),
            end_offset: Some(212),
            confidence: 0.9,
        })
        .await
        .expect("re-observation with a span");

    let states = store.list_world_states(Some("董卓")).await.expect("list");
    assert_eq!(states.len(), 1, "still a single row for one identity");
    assert_eq!(
        states[0].start_offset,
        Some(200),
        "a supplied start offset must replace the stored one"
    );
    assert_eq!(
        states[0].end_offset,
        Some(212),
        "a supplied end offset must replace the stored one"
    );
}

/// Objective: Verify the world-write parameter objects default to the values
/// their tables would have written, so a call site may omit a field without
/// silently storing a different number (the columns are always bound
/// explicitly, so the DDL default can never apply on its own).
/// Invariants: `NewWorldEvent` mirrors the `events` DDL (`event_type 'event'`,
/// `importance 0.5`); `NewWorldState` mirrors `world_states`
/// (`confidence 0.8`); every omitted span stays `None` instead of being guessed.
#[test]
fn new_world_rows_default_to_the_schema_values() {
    let event = NewWorldEvent::default();
    assert_eq!(
        event.event_type, "event",
        "events.event_type defaults to 'event'"
    );
    assert_eq!(event.importance, 0.5, "events.importance defaults to 0.5");
    assert_eq!(
        event.timestamp, None,
        "an omitted chapter must stay unknown"
    );
    assert_eq!(
        event.location, None,
        "an omitted location must stay unknown"
    );
    assert_eq!(
        event.start_offset, None,
        "an omitted span must stay unknown"
    );
    assert_eq!(
        event.end_offset, None,
        "an omitted end span must stay unknown"
    );
    assert!(
        event.title.is_empty() && event.description.is_empty(),
        "text fields default to empty, never to a placeholder"
    );

    let state = NewWorldState::default();
    assert_eq!(
        state.confidence, 0.8,
        "world_states.confidence defaults to 0.8"
    );
    assert_eq!(state.chapter, None, "an omitted chapter must stay unknown");
    assert_eq!(state.event_id, None, "no event anchor is invented");
    assert_eq!(
        state.start_offset, None,
        "an omitted span must stay unknown"
    );
    assert_eq!(
        state.end_offset, None,
        "an omitted end span must stay unknown"
    );
    assert!(
        state.entity_name.is_empty() && state.slot.is_empty() && state.value.is_empty(),
        "text fields default to empty, never to a placeholder"
    );
}
