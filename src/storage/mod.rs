//! Storage layer for the general knowledge model.
//!
//! This module owns the frozen schema DDL (see [`schema::KNOWLEDGE_SCHEMA`]).
//! The actual SQLite store implementation lives in [`crate::knowledge::store`];
//! keeping the DDL here separates the *shape* of the data from the *access*
//! logic, mirroring the `storage/schema.rs` layout called for in the dev guide.

pub mod schema;

pub use schema::KNOWLEDGE_SCHEMA;
pub use schema::WORLD_SCHEMA;
