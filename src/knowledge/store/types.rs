//! Row decoders and lightweight DTOs for the knowledge store.

use super::*;

pub(super) fn row_to_document(row: &rusqlite::Row) -> rusqlite::Result<Document> {
    Ok(Document {
        id: row.get("id")?,
        title: row.get("title")?,
        author: row.get("author")?,
        doc_type: row.get("doc_type")?,
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
    Ok(Evidence {
        id: row.get("id")?,
        doc_id: row.get("doc_id")?,
        chapter_id: row.get("chapter_id")?,
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
}

/// A V7 world-model relation row (`world_relations`).
#[derive(Debug, Clone)]
pub struct WorldRelation {
    pub source_id: i64,
    pub target_id: i64,
    pub relation_type: String,
    pub confidence: f64,
}
