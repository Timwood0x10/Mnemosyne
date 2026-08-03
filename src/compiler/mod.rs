//! # LoreScope World Model Compiler V7
//!
//! Converts narrative text into an Entity-centric world model.
//!
//! ## Pipeline
//!
//! ```text
//! Input (introductions + body text)
//!   │
//!   ├── Pass 1: World Builder (profiles → Entity nodes)
//!   └── Pass 2: Story Compiler (body text → Events → Relations)
//!   │
//!   └── Timeline Builder → Writer → Store
//! ```

pub mod alias;
pub mod chunk;
pub mod document;
pub mod extract;
pub mod faction;
pub mod inference;
pub mod name_validation;
pub mod pipeline;
pub mod profile;
pub mod pronoun;
pub mod relation;
pub mod resolver;
pub mod sentence;
pub mod timeline;
pub mod writer;

pub mod entity;

use std::ops::Range;

use serde::{Deserialize, Serialize};

// ── Chunk / Sentence (Phase 1-2, shared IR) ──────────────────────────────────

/// A chunk of text (parallel compilation unit).
#[derive(Debug, Clone)]
pub struct Chunk {
    pub index: usize,
    pub text: String,
    pub start_offset: usize,
    pub end_offset: usize,
    pub segment_num: i32,
    pub overlap_before: usize,
    pub overlap_after: usize,
}

/// A single sentence.
#[derive(Debug, Clone)]
pub struct Sentence {
    pub chunk_index: usize,
    pub index: usize,
    pub text: String,
    pub start_offset: usize,
    pub end_offset: usize,
}

/// Opaque id type for referencing sentences.
pub type SentenceId = usize;

// ── Entity (Pass 1) ─────────────────────────────────────────────────────────

/// A world entity (person, place, organization).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: Option<i64>,
    pub name: String,
    pub entity_type: String, // person / place / org / concept
    pub status: String,      // active / deceased / disbanded
    pub importance: f64,
}

/// A profile attribute attached to an entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityProfile {
    pub entity_id: Option<i64>,
    pub key: String, // courtesy_name, birthplace, appearance, etc.
    pub value: String,
    pub confidence: f64,
}

/// A mention of an entity in the text (Pass 2 builds these from Pass 1's index).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mention {
    pub sentence_id: usize,
    pub entity_id: Option<i64>, // None = candidate, resolved in Pass 2
    pub surface: String,        // "子龙"
    pub canonical_name: String, // "赵云"
    pub offset: Range<usize>,
    pub confidence: f64,
}

// ── Event (Pass 2) ──────────────────────────────────────────────────────────

/// An event in the world timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: Option<i64>,
    pub title: String,
    pub event_type: String,     // battle / dialogue / death / marriage / ...
    pub timestamp: Option<i32>, // chapter number or year
    pub location: Option<String>,
    pub description: String,
    pub participants: Vec<EventParticipant>,
    pub effects: Vec<EventEffect>,
    pub importance: f64,
}

/// A participant in an event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventParticipant {
    pub entity_name: String,
    pub role: String, // protagonist / antagonist / witness
}

/// A structured change that an event causes to an entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEffect {
    pub target: String,    // entity name affected
    pub attribute: String, // "status" / "reputation" / "relation" / "faction"
    pub old_value: Option<String>,
    pub new_value: String, // "deceased" / "+" / "-" / faction name
    pub confidence: f64,
}

/// A long-term relation between two entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    pub source: String,
    pub target: String,
    pub relation_type: String,   // brother / enemy / teacher / spouse
    pub valid_from: Option<i32>, // event timestamp
    pub valid_to: Option<i32>,
    pub confidence: f64,
}

// ── Evidence ─────────────────────────────────────────────────────────────────

/// A slice of the original text that supports a fact.
#[derive(Debug, Clone)]
pub struct EvidenceSlice {
    pub text: String,
    pub sentence_id: usize,
    pub segment_num: i32,
    pub offset_start: usize,
    pub offset_end: usize,
}

// ── CompileContext ───────────────────────────────────────────────────────────

/// Shared context — flows through the entire pipeline.
#[derive(Debug, Default)]
pub struct CompileContext {
    // Input
    pub document_title: String,

    // Pass 1: World Builder outputs
    pub entities: Vec<Entity>,
    pub profiles: Vec<EntityProfile>,

    // Pass 1: Alias index
    pub mentions: Vec<Mention>,

    // Pass 2: Story Compiler outputs
    pub events: Vec<Event>,
    pub relations: Vec<Relation>,

    // Timing
    pub current_timestamp: Option<i32>,
}

/// Compilation statistics.
#[derive(Debug, Clone, Default)]
pub struct CompileStats {
    pub entities: usize,
    pub profiles: usize,
    pub mentions: usize,
    pub events: usize,
    pub relations: usize,
    pub evidence_slices: usize,
}
