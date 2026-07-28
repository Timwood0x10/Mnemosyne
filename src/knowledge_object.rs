/// Knowledge Object Model for the LoreScope Knowledge Compiler.
///
/// All entities (persons, events, locations, concepts, etc.) are stored as
/// generic `KnowledgeObject`, connected by typed `KnowledgeEdge`s with
/// traceable `Evidence`. This unifies the previous Character/Event/Relation
/// domain models under a single extensible framework.

use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[cfg(feature = "chrono")]
use chrono::{DateTime, Utc};

/// Represents any object in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ObjectType {
    /// A person/character
    Person,
    /// An organization/faction/clan
    Organization,
    /// A location/place
    Location,
    /// An artifact/object/item
    Artifact,
    /// An abstract concept/idea
    Concept,
    /// An emotion/state
    Emotion,
    /// A narrative event (battle, marriage, death, etc.)
    Event,
    /// A relationship between objects (acts as an edge)
    Relationship,
}

/// Generic knowledge object that can represent any type of entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeObject {
    /// Unique ID (UUID)
    pub id: String,
    /// Object type (Person, Event, Location, ...)
    pub object_type: ObjectType,
    /// Display name/title
    pub title: String,
    /// Document/novel source this object belongs to
    pub document_id: String,
    /// Canonical/formal name (e.g., "诸葛亮" vs aliases like "孔明")
    pub canonical_name: String,
    /// Additional attributes as JSON (faction, importance, role, etc.)
    pub attributes: serde_json::Map<String, serde_json::Value>,
    /// Confidence score of this object's existence/recognition [0.0-1.0]
    pub confidence: f64,
    /// When this object was first observed (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
}

impl KnowledgeObject {
    /// Create a new person object.
    pub fn person(
        title: &str,
        document_id: &str,
        canonical_name: Option<&str>,
        attributes: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        let obj_type = ObjectType::Person;
        KnowledgeObject {
            id: format!("obj_{}", Uuid::new_v4()),
            object_type: obj_type,
            title: title.to_string(),
            document_id: document_id.to_string(),
            canonical_name: canonical_name.unwrap_or(title).to_string(),
            attributes,
            confidence: 1.0,
            created_at: Some(Utc::now()),
        }
    }

    /// Create a new event object.
    pub fn event(
        title: &str,
        document_id: &str,
        event_type: &str,
        attributes: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        let obj_type = ObjectType::Event;
        KnowledgeObject {
            id: format!("obj_{}", Uuid::new_v4()),
            object_type: obj_type,
            title: title.to_string(),
            document_id: document_id.to_string(),
            canonical_name: title.to_string(),
            attributes: {
                let mut a = attributes;
                a.insert("event_type".into(), serde_json::Value::String(event_type.into()));
                a
            },
            confidence: 1.0,
            created_at: Some(Utc::now()),
        }
    }

    /// Check if two objects are logically equal (same ID or same canonical name in same document).
    pub fn is_equal(&self, other: &Self) -> bool {
        self.id == other.id || (self.canonical_name == other.canonical_name && self.document_id == other.document_id)
    }
}

/// A directed edge between two knowledge objects with a typed predicate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEdge {
    /// Unique edge ID (UUID)
    pub id: String,
    /// Source object reference
    pub source: String, // ref to KnowledgeObject.id
    /// Target object reference
    pub target: String, // ref to KnowledgeObject.id
    /// Predicate/relation type (e.g., "participated_in", "trusts", "brother_of")
    pub predicate: String,
    /// Edge properties (weight, confidence, start_chapter, end_chapter, etc.)
    pub properties: serde_json::Map<String, serde_json::Value>,
    /// When this relation was established (chapter number, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_chapter: Option<i32>,
    /// When this relation ended (NULL for ongoing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_chapter: Option<i32>,
    /// Creation timestamp
    pub created_at: DateTime<Utc>,
}

impl KnowledgeEdge {
    /// Create a new edge with default properties.
    pub fn new(
        source: &str,
        target: &str,
        predicate: &str,
        properties: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Self {
        let mut props = properties.unwrap_or_else(|| serde_json::Map::new());
        props.insert("confidence".into(), serde_json::Value::Number(serde_json::Number::from_f64(0.5).unwrap()));
        
        KnowledgeEdge {
            id: format!("edge_{}", Uuid::new_v4()),
            source: source.to_string(),
            target: target.to_string(),
            predicate: predicate.into(),
            properties: props,
            start_chapter: None,
            end_chapter: None,
            created_at: Utc::now(),
        }
    }

    /// Get the weight/score from edge properties.
    pub fn weight(&self) -> f64 {
        self.properties.get("weight").and_then(|v| v.as_f64()).unwrap_or(0.5)
    }

    /// Get confidence from edge properties.
    pub fn confidence(&self) -> f64 {
        self.properties.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.5)
    }
}

/// Evidence linking a knowledge fact to its source in the original text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    /// Unique evidence ID
    pub id: String,
    /// The object this evidence supports (can be object OR edge id)
    pub referenced_id: String,
    /// Type of evidence: direct (quoted text), inferred (derived via rule), etc.
    pub evidence_type: EvidenceType,
    /// The actual text snippet that serves as evidence
    pub content: String,
    /// Chapter where this evidence appears
    pub chapter: i32,
    /// Sentence-level position within the chapter (for granular tracing)
    pub sentence_id: u64,
    /// Byte offset in original document
    pub start_offset: usize,
    pub end_offset: usize,
    /// Confidence level of this evidence [0.0-1.0]
    pub confidence: f64,
    /// When created
    pub created_at: DateTime<Utc>,
}

/// Type of evidence source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum EvidenceType {
    /// Direct quote from text (explicit statement)
    Direct,
    /// Derived from event pattern via inference rules
    Inferred,
    /// Deduced from dialogue context or pronoun resolution
    Derived,
    /// Based on co-occurrence statistics
    Statistical,
}

impl Evidence {
    /// Create a new direct evidence item.
    pub fn direct(
        referenced_id: &str,
        content: &str,
        chapter: i32,
        sentence_id: u64,
        start_offset: usize,
        end_offset: usize,
        confidence: f64,
    ) -> Self {
        Evidence {
            id: format!("ev_{}", Uuid::new_v4()),
            referenced_id: referenced_id.into(),
            evidence_type: EvidenceType::Direct,
            content: content.into(),
            chapter,
            sentence_id,
            start_offset,
            end_offset,
            confidence,
            created_at: Utc::now(),
        }
    }
}

/// A node in the knowledge graph, wrapping a KnowledgeObject with its related edges and evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeGraphNode {
    /// The main object
    pub object: KnowledgeObject,
    /// Outgoing edges (relationships FROM this object)
    pub outgoing_edges: Vec<KnowledgeEdge>,
    /// Incoming edges (relationships TO this object)
    pub incoming_edges: Vec<KnowledgeEdge>,
    /// Evidence supporting facts about this object
    pub evidences: Vec<Evidence>,
    /// Connected neighbor nodes (for graph traversal)
    pub neighbors: Vec<KnowledgeGraphNode>,
}

impl KnowledgeGraphNode {
    /// Build a node from a KnowledgeObject and related data.
    pub fn new(object: KnowledgeObject) -> Self {
        KnowledgeGraphNode {
            object,
            outgoing_edges: vec![],
            incoming_edges: vec![],
            evidences: vec![],
            neighbors: vec![],
        }
    }

    /// Add an edge (both source->target and target->source will be reflected appropriately).
    pub fn add_edge(&mut self, edge: KnowledgeEdge, is_outgoing: bool) {
        if is_outgoing {
            self.outgoing_edges.push(edge);
        } else {
            self.incoming_edges.push(edge);
        }
    }

    /// Add evidence for this object.
    pub fn add_evidence(&mut self, evidence: Evidence) {
        self.evidences.push(evidence);
    }

    /// Add a neighbor node (for graph traversal representation).
    pub fn add_neighbor(&mut self, neighbor: KnowledgeGraphNode) {
        self.neighbors.push(neighbor);
    }
}
