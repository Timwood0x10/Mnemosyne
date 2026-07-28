//! General knowledge model — Object + Edge + Evidence.
//!
//! This is the core of LoreScope's "Narrative World Compiler" (dev_guide §3,
//! V2.0 冻结版). The model is deliberately generic: every entity is a
//! [`KnowledgeObject`], every relationship a [`KnowledgeEdge`], and every
//! claim is backed by [`Evidence`] linked through [`KnowledgeEvidenceLink`].
//! No domain sub-tables (no `CharacterObject` / `EventObject`): all extra
//! attributes live in the `properties` JSON bag (dev_guide §3.4 "不再加字段").
//!
//! Knowledge is split into `observed` (stated by the source text) and
//! `derived` (inferred by a rule engine) via [`Origin`], and edges carry
//! temporal bounds (`valid_from` / `valid_to` as chapter numbers) so that
//! relations which change over the narrative — e.g. 吕布→丁原 `serves` ch1 →
//! `kills` ch3 — do not pollute time-scoped queries (dev_guide §3.5).
//!
//! The schema DDL lives in [`crate::storage::schema`]; the SQLite store and
//! high-level queries live in [`store`] and the V1→general migrator in
//! [`migration`] (both registered as submodules once implemented).

use serde::{Deserialize, Serialize};

pub mod migration;
pub mod store;

pub use migration::{MigrationStats, Migrator};
pub use store::{KnowledgeStore, SQLiteKnowledgeStore};

// ───────────────────────────────────────────────────────────────────────────
// Enumerations — drive the SQL CHECK / type columns.
// ───────────────────────────────────────────────────────────────────────────

/// Kind of knowledge object. Matches the `object_type` column values from
/// dev_guide §3.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectType {
    Person,
    Event,
    Place,
    Organization,
    Concept,
    Artifact,
    Role,
}

impl ObjectType {
    /// Stable string stored in the `object_type` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ObjectType::Person => "person",
            ObjectType::Event => "event",
            ObjectType::Place => "place",
            ObjectType::Organization => "organization",
            ObjectType::Concept => "concept",
            ObjectType::Artifact => "artifact",
            ObjectType::Role => "role",
        }
    }
}

impl std::str::FromStr for ObjectType {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "person" => Ok(ObjectType::Person),
            "event" => Ok(ObjectType::Event),
            "place" => Ok(ObjectType::Place),
            "organization" => Ok(ObjectType::Organization),
            "concept" => Ok(ObjectType::Concept),
            "artifact" => Ok(ObjectType::Artifact),
            "role" => Ok(ObjectType::Role),
            other => Err(format!("unknown object type: {other}")),
        }
    }
}

/// Provenance of a knowledge edge (dev_guide §3.5 `origin` column).
///
/// - `Observed`: stated explicitly in the source text.
/// - `Derived`: inferred by the rule engine (dev_guide §4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Observed,
    Derived,
}

impl Origin {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Observed => "observed",
            Origin::Derived => "derived",
        }
    }
}

impl std::str::FromStr for Origin {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "observed" => Ok(Origin::Observed),
            "derived" => Ok(Origin::Derived),
            other => Err(format!("unknown origin: {other}")),
        }
    }
}

/// Which kind of knowledge row an evidence link backs (dev_guide §3.7
/// `source_type` column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceSourceType {
    Object,
    Edge,
}

impl EvidenceSourceType {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceSourceType::Object => "object",
            EvidenceSourceType::Edge => "edge",
        }
    }
}

impl std::str::FromStr for EvidenceSourceType {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "object" => Ok(EvidenceSourceType::Object),
            "edge" => Ok(EvidenceSourceType::Edge),
            other => Err(format!("unknown evidence source type: {other}")),
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Row structs — one per frozen table.
// ───────────────────────────────────────────────────────────────────────────

/// A document (e.g. a novel). Maps to the `documents` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: i64,
    pub title: String,
    pub author: Option<String>,
    pub doc_type: Option<String>,
    pub created_at: i64,
}

/// A chapter within a document. Maps to the `chapters` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    pub id: i64,
    pub doc_id: i64,
    pub chapter_no: i32,
    pub title: Option<String>,
    pub content: String,
    pub start_offset: Option<i64>,
    pub end_offset: Option<i64>,
}

/// A knowledge entity (person/event/place/...). Maps to `knowledge_objects`.
///
/// All attributes beyond `object_type`/`name` live in `properties` — e.g. for
/// a person: `aliases`, `clothing`, `personality`, `description`, `importance`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeObject {
    pub id: i64,
    pub doc_id: i64,
    pub object_type: ObjectType,
    pub name: String,
    pub properties: serde_json::Value,
    pub confidence: f64,
    pub created_at: i64,
}

/// A directed, temporal relationship between two objects. Maps to
/// `knowledge_edges`. `valid_from`/`valid_to` are chapter numbers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEdge {
    pub id: i64,
    pub source_id: i64,
    pub target_id: i64,
    pub predicate: String,
    pub properties: serde_json::Value,
    pub origin: Origin,
    pub confidence: f64,
    pub valid_from: Option<i32>,
    pub valid_to: Option<i32>,
    pub created_at: i64,
}

/// An original-text snippet that backs one or more facts. Maps to `evidence`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub id: i64,
    pub doc_id: i64,
    pub chapter_id: i64,
    pub start_offset: Option<i64>,
    pub end_offset: Option<i64>,
    pub content: String,
    pub created_at: i64,
}

/// A many-to-many link between a knowledge row and the evidence backing it.
/// Maps to `knowledge_evidence`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEvidenceLink {
    pub id: i64,
    pub source_type: EvidenceSourceType,
    pub source_id: i64,
    pub evidence_id: i64,
}

/// An entity-mention position index. Maps to `mentions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mention {
    pub id: i64,
    pub object_id: i64,
    pub chapter_id: i64,
    pub start_offset: Option<i64>,
    pub end_offset: Option<i64>,
    pub alias_used: Option<String>,
    pub confidence: f64,
}

/// A lore-compiler build record. Maps to `compiler_runs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilerRun {
    pub id: i64,
    pub doc_id: i64,
    pub version: String,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub status: Option<String>,
    pub statistics: Option<serde_json::Value>,
}

// ───────────────────────────────────────────────────────────────────────────
// Result DTOs — shapes returned by the high-level queries / MCP tools.
// ───────────────────────────────────────────────────────────────────────────

/// Result of `inspect_entity` (dev_guide §5): an object together with the
/// events it participates in, the relations it has, the evidence backing
/// those facts, and where it is mentioned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectEntityResult {
    pub object: KnowledgeObject,
    /// Profile attributes (key-value pairs like courtesy_name, birthplace).
    pub profile: Vec<EntityProfileEntry>,
    /// Event-type objects the entity participates in (via `participated_in` edges).
    pub events: Vec<KnowledgeObject>,
    /// Person↔person (and other non-event) edges touching the entity.
    pub relations: Vec<KnowledgeEdge>,
    pub evidences: Vec<Evidence>,
    pub mentions: Vec<Mention>,
    /// Lifecycle: birth chapter → peak chapters → death chapter.
    pub lifecycle: EntityLifecycle,
    /// Character arc over time (if available).
    pub character_arc: Option<String>,
}

/// A single profile key-value entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityProfileEntry {
    pub key: String,
    pub value: String,
    pub confidence: f64,
}

/// Entity lifecycle summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityLifecycle {
    /// First appearance chapter.
    pub first_seen: Option<i32>,
    /// Last appearance chapter.
    pub last_seen: Option<i32>,
    /// Death chapter (if entity status is deceased).
    pub death_chapter: Option<i32>,
    /// Number of events this entity participated in.
    pub event_count: usize,
}

/// One row of the `timeline` tool (dev_guide §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub chapter: i32,
    pub event: String,
    pub predicate: String,
    pub target: String,
}

/// A node in the `relation_graph` result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: i64,
    pub name: String,
    pub object_type: ObjectType,
}

/// An edge in the `relation_graph` result, including temporal bounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub source_id: i64,
    pub target_id: i64,
    pub predicate: String,
    pub valid_from: Option<i32>,
    pub valid_to: Option<i32>,
    pub confidence: f64,
}

/// Result of `relation_graph` (dev_guide §5): the BFS subgraph around an
/// entity up to the requested depth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationGraphResult {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// One hit from the `evidence` search tool (dev_guide §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceHit {
    pub text: String,
    pub chapter: i32,
    pub doc: String,
    pub confidence: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify ObjectType as_str/FromStr round-trip for every variant.
    /// Invariants: Each variant's string parses back to the same variant.
    #[test]
    fn object_type_round_trip() {
        for (variant, s) in [
            (ObjectType::Person, "person"),
            (ObjectType::Event, "event"),
            (ObjectType::Place, "place"),
            (ObjectType::Organization, "organization"),
            (ObjectType::Concept, "concept"),
            (ObjectType::Artifact, "artifact"),
            (ObjectType::Role, "role"),
        ] {
            assert_eq!(variant.as_str(), s, "as_str mismatch for {s}");
            let parsed: ObjectType = s.parse().unwrap_or_else(|e| panic!("parse {s}: {e}"));
            assert_eq!(parsed, variant, "round-trip mismatch for {s}");
        }
    }

    /// Objective: Verify Origin round-trip and rejection of unknown values.
    /// Invariants: observed/derived parse back; unknown strings error.
    #[test]
    fn origin_round_trip_and_reject() {
        assert_eq!(Origin::Observed.as_str(), "observed");
        assert_eq!(Origin::Derived.as_str(), "derived");
        assert_eq!("observed".parse::<Origin>().unwrap(), Origin::Observed);
        assert_eq!("Derived".parse::<Origin>().unwrap(), Origin::Derived);
        assert!("guessed".parse::<Origin>().is_err());
    }

    /// Objective: Verify EvidenceSourceType round-trip.
    /// Invariants: object/edge parse back; case-insensitive.
    #[test]
    fn evidence_source_type_round_trip() {
        assert_eq!(
            "object".parse::<EvidenceSourceType>().unwrap(),
            EvidenceSourceType::Object
        );
        assert_eq!(
            "EDGE".parse::<EvidenceSourceType>().unwrap(),
            EvidenceSourceType::Edge
        );
        assert!("sentence".parse::<EvidenceSourceType>().is_err());
    }
}
