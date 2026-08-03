//! # Memory Distillation MCP Server
//!
//! An independent MCP (Model Context Protocol) server that extracts and distills
//! memories from conversations. Uses sqlite-vec for vector storage and search.
//!
//! ## Architecture
//!
//! The server exposes 10 MCP tools (`memory_distill`, `memory_compile`,
//! `memory_search`, `memory_store`, `memory_feedback`, `memory_stats`,
//! `character_search`, `character_network`, `character_ingest`,
//! `character_graph`) backed by an 8-stage distillation pipeline, a
//! SQLite-vec vector store, and a character knowledge graph store.
//!
//! ## Modules
//!
//! | Module | Responsibility |
//! |--------|---------------|
//! | `error` | Unified error types |
//! | `types` | Core DTOs: Memory, Experience, Message |
//! | `detector` | Question detection / IsProblem |
//! | `filter` | Noise + Security filters |
//! | `classifier` | Memory type classification |
//! | `scorer` | Importance scoring |
//! | `extractor` | Problem-Solution pair extraction |
//! | `resolver` | Conflict detection and resolution |
//! | `embed` | EmbeddingService trait + remote impl |
//! | `store` | ExperienceRepository trait + sqlite-vec impl |
//! | `distiller` | 8-stage distillation pipeline orchestrator |
//! | `character` | Character knowledge graph store + network traversal |
//! | `ingest` | Character corpus distillation (Python `ingest_characters.py` port) |
//! | `config` | Configuration loading |
//! | `mcp` | MCP server framework (types, transport, server) |

pub mod agent_facts;
pub mod agent_personality;
pub mod anchor;
pub mod centroid;
pub mod character;
pub mod classifier;
pub mod cognition;
pub mod cognition_compiler;
pub mod compiler;
pub mod config;
pub mod conversation_compiler;
pub mod detector;
pub mod dictionary;
pub mod distiller;
pub mod embed;
pub mod entity_resolver;
pub mod error;
pub mod extractor;
pub mod fact_store;
pub mod faction;
pub mod filter;
pub mod fused_compile;
pub mod ingest;
pub mod knowledge;
pub mod language;
pub mod lexicon;
pub mod mcp;
pub mod observation_compiler;
pub mod personality;
pub mod prompt;
pub mod resolver;
pub mod retrieval;
pub mod scorer;
pub mod storage;
pub mod store;
pub mod temporal;
pub mod types;
pub mod value_extract;
pub mod vector;
