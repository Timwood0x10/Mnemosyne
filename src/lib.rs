//! # Memory Distillation MCP Server
//!
//! An independent MCP (Model Context Protocol) server that extracts and distills
//! memories from conversations. Uses sqlite-vec for vector storage and search.
//!
//! ## Architecture
//!
//! The server exposes 5 MCP tools (`distill`, `search`, `list`, `delete`, `stats`)
//! backed by an 8-stage distillation pipeline and a SQLite-vec vector store.
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
//! | `config` | Configuration loading |
//! | `mcp` | MCP server framework (types, transport, server) |

pub mod classifier;
pub mod config;
pub mod detector;
pub mod distiller;
pub mod embed;
pub mod error;
pub mod extractor;
pub mod filter;
pub mod mcp;
pub mod resolver;
pub mod retrieval;
pub mod scorer;
pub mod store;
pub mod types;
