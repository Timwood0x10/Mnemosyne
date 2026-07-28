//! # LoreScope Compiler Pipeline
//!
//! Converts unstructured text into a structured, evidence-backed knowledge
//! graph of objects, relations, and evidence slices.
//!
//! ## Pipeline stages
//!
//! ```text
//! Text → Document → Sentence → Entity → Resolver → Observation
//!   → ObjectBuilder + EdgeBuilder → Rule → Writer
//! ```
//!
//! Each stage reads from (and writes to) the shared [`CompileContext`].
//! Nothing is persisted until [`writer`] writes the final [`CompileResult`]
//! into a [`KnowledgeWriter`](crate::knowledge::store::KnowledgeWriter).

pub mod alias;
pub mod builder;
pub mod chunk;
pub mod document;
pub mod inference;
pub mod merge;
pub mod observation;
pub mod pipeline;
pub mod pronoun;
pub mod sentence;
pub mod writer;

pub mod entity;

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::knowledge::Origin;

// ── CompileContext ───────────────────────────────────────────────────────────

/// Shared context carried through the compiler pipeline.
///
/// Each stage fills in its corresponding field. At the end, [`Self::result`]
/// holds the final [`CompileResult`] ready for the writer.
#[derive(Debug, Default)]
pub struct CompileContext {
    pub document: Option<document::Document>,
    pub chunks: Vec<Chunk>,
    pub sentences: Vec<Sentence>,
    pub mentions: Vec<Mention>,
    pub observations: Vec<Observation>,
    pub result: Option<CompileResult>,
}

// ── Chunk (Phase 1) ──────────────────────────────────────────────────────────

/// A chunk of the input document produced by [`ChunkPlanner`](crate::compiler::chunk).
///
/// Chunks are the unit of parallel compilation. Each chunk carries overlap
/// metadata so that downstream resolvers can resolve cross-chunk references.
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

// ── Sentence (Phase 2) ──────────────────────────────────────────────────────

/// A single sentence split from a [`Chunk`].
#[derive(Debug, Clone)]
pub struct Sentence {
    pub chunk_index: usize,
    pub index: usize,       // position within the chunk
    pub text: String,
    pub start_offset: usize,
    pub end_offset: usize,
}

/// Opaque id type for referencing sentences within a compile run.
pub type SentenceId = usize;

// ── Mention (Phase 3) ───────────────────────────────────────────────────────

/// A mention of an entity in the text (IR, not persisted).
///
/// `entity_id` is NOT stored here — it is assigned by
/// [`crate::knowledge::store::KnowledgeWriter`] during UPSERT.
#[derive(Debug, Clone)]
pub struct Mention {
    pub sentence_id: SentenceId,
    pub surface: String,         // "子龙"
    pub canonical_name: String,  // "赵云"
    pub offset: Range<usize>,
    pub confidence: f64,
}

// ── ResolvedMention (Phase 4) ───────────────────────────────────────────────

/// A [`Mention`] after coreference resolution.
#[derive(Debug, Clone)]
pub struct ResolvedMention {
    pub mention: Mention,
    pub resolved_to: String,     // "他" → "赵云"
    pub strategy: ResolveStrategy,
}

/// How a mention was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveStrategy {
    /// Identity — the surface form is already the canonical name.
    Identity,
    /// Resolved by a pronoun ("他" / "她" / "其").
    Pronoun,
    /// Resolved by a title or role ("主公" / "先生" / "将军").
    Title,
    /// Resolved by context (nearest preceding subject).
    Context,
}

// ── Observation (Phase 5) ───────────────────────────────────────────────────

/// A subject–predicate–object observation extracted from a single sentence.
///
/// This is the central IR of the compiler. Everything downstream derives from
/// these triples. Observations are NOT persisted — they are intermediate
/// representations.
#[derive(Debug, Clone)]
pub struct Observation {
    pub sentence_id: SentenceId,
    pub predicate: String,
    pub arguments: Vec<Argument>,
    pub confidence: f64,
}

/// A typed argument in an [`Observation`].
#[derive(Debug, Clone)]
pub struct Argument {
    pub role: SemanticRole,
    pub value: String,
}

/// Semantic roles for observation arguments.
///
/// V1 only covers the most common roles. `Other` allows extension without
/// changing the struct.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SemanticRole {
    Subject,
    Object,
    Recipient,
    Instrument,
    Location,
    Time,
    Modifier,
    Other(String),
}

// ── CompileResult (Phase 6) ─────────────────────────────────────────────────

/// Final output of the compiler pipeline, ready for the [`writer`].
///
/// NOT persisted directly — the writer converts these into
/// [`KnowledgeObject`], [`KnowledgeEdge`], and [`Evidence`] before storing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompileResult {
    pub objects: Vec<CompiledObject>,
    pub edges: Vec<CompiledEdge>,
    pub stats: CompileStats,
}

/// A compiled object (IR form, before storage conversion).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledObject {
    pub name: String,
    pub object_type: String,
    pub properties: std::collections::HashMap<String, String>,
    pub evidence: EvidenceSlice,
}

/// A compiled edge (IR form, before storage conversion).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledEdge {
    pub source: String,
    pub predicate: String,
    pub target: String,
    pub origin: Origin,
    pub confidence: f64,
    pub evidence: EvidenceSlice,
}

/// A slice of the original text that supports a fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceSlice {
    pub text: String,
    pub sentence_id: SentenceId,
    pub segment_num: i32,
    pub offset_start: usize,
    pub offset_end: usize,
}

/// Compilation statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompileStats {
    pub sentences: usize,
    pub mentions: usize,
    pub observations: usize,
    pub objects: usize,
    pub edges: usize,
    pub evidence_slices: usize,
    pub derived_edges: usize,
}
