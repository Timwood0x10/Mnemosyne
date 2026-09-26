//! Row decoders and lightweight DTOs for the knowledge store.

use super::*;

pub(super) fn row_to_document(row: &rusqlite::Row) -> rusqlite::Result<Document> {
    Ok(Document {
        id: row.get("id")?,
        title: row.get("title")?,
        author: row.get("author")?,
        doc_type: row.get("doc_type")?,
        source: row.get("source")?,
        created_at: row.get("created_at")?,
    })
}

/// Convert a rusqlite row into a [`Chapter`].
pub(super) fn row_to_chapter(row: &rusqlite::Row) -> rusqlite::Result<Chapter> {
    Ok(Chapter {
        id: row.get("id")?,
        doc_id: row.get("doc_id")?,
        chapter_no: row.get("chapter_no")?,
        title: row.get("title")?,
        content: row.get("content")?,
        start_offset: row.get("start_offset")?,
        end_offset: row.get("end_offset")?,
    })
}

/// Convert a rusqlite row into a [`KnowledgeObject`], parsing the JSON
/// `properties` bag defensively (bad JSON → empty object).
pub(super) fn row_to_object(row: &rusqlite::Row) -> rusqlite::Result<KnowledgeObject> {
    let type_str: String = row.get("object_type")?;
    let props_str: String = row.get("properties").unwrap_or_default();
    let object_type = type_str.parse::<ObjectType>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::<std::io::Error>::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;
    Ok(KnowledgeObject {
        id: row.get("id")?,
        doc_id: row.get("doc_id")?,
        object_type,
        name: row.get("name")?,
        properties: serde_json::from_str(&props_str)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
        confidence: row.get("confidence")?,
        created_at: row.get("created_at")?,
    })
}

/// Convert a rusqlite row into a [`KnowledgeEdge`].
pub(super) fn row_to_edge(row: &rusqlite::Row) -> rusqlite::Result<KnowledgeEdge> {
    let origin_str: String = row.get("origin")?;
    let props_str: String = row.get("properties").unwrap_or_default();
    let origin = origin_str.parse::<Origin>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::<std::io::Error>::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;
    Ok(KnowledgeEdge {
        id: row.get("id")?,
        source_id: row.get("source_id")?,
        target_id: row.get("target_id")?,
        predicate: row.get("predicate")?,
        properties: serde_json::from_str(&props_str)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
        origin,
        confidence: row.get("confidence")?,
        valid_from: row.get("valid_from")?,
        valid_to: row.get("valid_to")?,
        created_at: row.get("created_at")?,
    })
}

/// Convert a rusqlite row into an [`Evidence`].
pub(super) fn row_to_evidence(row: &rusqlite::Row) -> rusqlite::Result<Evidence> {
    // `doc_id`/`chapter_id` are nullable (shared schema with the fact store);
    // a fact-anchor row with NULL chapter must decode as 0 rather than
    // InvalidColumnType, so knowledge queries can list it without failing.
    Ok(Evidence {
        id: row.get("id")?,
        doc_id: row.get::<_, Option<i64>>("doc_id")?.unwrap_or(0),
        chapter_id: row.get::<_, Option<i64>>("chapter_id")?.unwrap_or(0),
        start_offset: row.get("start_offset")?,
        end_offset: row.get("end_offset")?,
        content: row.get("content")?,
        created_at: row.get("created_at")?,
    })
}

/// Convert a rusqlite row into a [`Mention`].
pub(super) fn row_to_mention(row: &rusqlite::Row) -> rusqlite::Result<Mention> {
    Ok(Mention {
        id: row.get("id")?,
        object_id: row.get("object_id")?,
        chapter_id: row.get("chapter_id")?,
        start_offset: row.get("start_offset")?,
        end_offset: row.get("end_offset")?,
        alias_used: row.get("alias_used")?,
        confidence: row.get("confidence")?,
    })
}

/// Serialize a `serde_json::Value` for the `properties`/`statistics` JSON
/// columns. A null value is stored as the empty object so the column default
/// semantics are preserved.
pub(super) fn json_to_string(v: &serde_json::Value) -> String {
    if v.is_null() {
        "{}".to_string()
    } else {
        serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Aggregate counts over the knowledge graph, for memory-health reporting.
#[derive(Debug, Clone, Serialize)]
pub struct GraphCounts {
    pub documents: usize,
    pub objects: usize,
    pub edges: usize,
    pub evidence: usize,
}

/// A V7 world-model entity row (`world_entities`).
#[derive(Debug, Clone)]
pub struct WorldEntity {
    pub id: i64,
    pub name: String,
    pub entity_type: String,
    pub importance: f64,
}

/// A V7 world-model profile row (`world_entity_profiles`).
#[derive(Debug, Clone)]
pub struct WorldProfile {
    pub entity_id: i64,
    pub key: String,
    pub value: String,
    pub confidence: f64,
    /// Evidence row anchoring this claim to a source byte span.
    pub evidence_id: Option<i64>,
}

/// A V7 world-model relation row (`world_relations`).
#[derive(Debug, Clone)]
pub struct WorldRelation {
    pub source_id: i64,
    pub target_id: i64,
    pub relation_type: String,
    pub confidence: f64,
}

/// A V7 world-model event row (`events`), including its source byte span.
///
/// `start_offset`/`end_offset` are absolute positions in the original
/// document (dialogue = whole sentence; action = verb-match window). `None`
/// for legacy rows written before the columns existed.
#[derive(Debug, Clone)]
pub struct WorldEvent {
    pub id: i64,
    pub title: String,
    pub event_type: String,
    pub timestamp: Option<i32>,
    pub location: Option<String>,
    pub description: String,
    pub importance: f64,
    pub start_offset: Option<i64>,
    pub end_offset: Option<i64>,
}

/// A V7 character-state slot row (`world_states`).
///
/// One row = one observation of `(entity, slot)` at a narrative time,
/// anchored to the event that produced it and the source byte span.
/// The *current* state of a slot is the row with the highest
/// `(chapter, id)` — never overwritten in place (ADD-only).
#[derive(Debug, Clone)]
pub struct WorldState {
    pub id: i64,
    pub entity_name: String,
    pub slot: String,
    pub value: String,
    pub chapter: Option<i32>,
    pub event_id: Option<i64>,
    pub start_offset: Option<i64>,
    pub end_offset: Option<i64>,
    pub confidence: f64,
}

/// One `event_participants` row resolved to its entity name — the portable
/// form (numeric `event_id`/`entity_id` are local to each database, so
/// export carries name + role only).
#[derive(Debug, Clone)]
pub struct EventParticipantRef {
    /// World-entity name (upserted by name on import).
    pub entity_name: String,
    /// Role (`subject` / `object` / `speaker` / `participant` / …).
    pub role: String,
}

/// The fields that identify and describe a world event to insert (`events`).
///
/// Mirrors [`WorldEvent`] minus the assigned `id`. It exists because the upsert
/// used to take eight positional arguments — five of them optional spans or
/// numbers — which needed `#[allow(clippy::too_many_arguments)]` to pass and
/// left every call site to be read argument by argument.
#[derive(Debug, Clone, Copy)]
pub struct NewWorldEvent<'a> {
    /// Event title. Together with `timestamp` and the byte span it forms the
    /// upsert identity.
    pub title: &'a str,
    /// Free-form type (`event` / `battle` / `dialogue` / `death` / …).
    pub event_type: &'a str,
    /// Narrative time (chapter or year), when known.
    pub timestamp: Option<i32>,
    /// Where it happened, when known.
    pub location: Option<&'a str>,
    /// One-line description — usually the source sentence.
    pub description: &'a str,
    /// Importance in `[0, 1]`.
    pub importance: f64,
    /// Source byte span start; part of the identity, so the same title at two
    /// spans stays two events.
    pub start_offset: Option<i64>,
    /// Source byte span end; part of the identity.
    pub end_offset: Option<i64>,
}

impl<'a> Default for NewWorldEvent<'a> {
    /// Mirrors the `events` DDL defaults (`event_type 'event'`,
    /// `importance 0.5`), so omitting a field at a call site writes what a SQL
    /// insert that omitted it would have written. `importance` is always bound
    /// explicitly, so the column default never applies on its own.
    fn default() -> Self {
        Self {
            title: "",
            event_type: "event",
            timestamp: None,
            location: None,
            description: "",
            importance: 0.5,
            start_offset: None,
            end_offset: None,
        }
    }
}

/// The fields that identify and describe a character-state slot to insert
/// (`world_states`).
///
/// Mirrors [`WorldState`] minus the assigned `id`, and exists for the same
/// reason as [`NewWorldEvent`]: the upsert took eight positional arguments.
#[derive(Debug, Clone, Copy)]
pub struct NewWorldState<'a> {
    /// Entity whose slot this is; upserted into `world_entities` when unseen.
    pub entity_name: &'a str,
    /// Slot name (`status` / `location` / …).
    pub slot: &'a str,
    /// The observed value (`deceased` / `captured` / …).
    pub value: &'a str,
    /// Narrative time (chapter or year), when known.
    pub chapter: Option<i32>,
    /// Source event anchoring this observation; `None` for a manual write.
    pub event_id: Option<i64>,
    /// Source byte span start, when known.
    pub start_offset: Option<i64>,
    /// Source byte span end, when known.
    pub end_offset: Option<i64>,
    /// Confidence in `[0, 1]`.
    pub confidence: f64,
}

impl<'a> Default for NewWorldState<'a> {
    /// Mirrors the `world_states` DDL default (`confidence 0.8`), which can
    /// never apply on its own: the column is always bound explicitly.
    fn default() -> Self {
        Self {
            entity_name: "",
            slot: "",
            value: "",
            chapter: None,
            event_id: None,
            start_offset: None,
            end_offset: None,
            confidence: 0.8,
        }
    }
}
