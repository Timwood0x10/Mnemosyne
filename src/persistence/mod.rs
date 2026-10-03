//! The persistence layer shared by every store.
//!
//! This module owns the frozen schema DDL (see [`schema::KNOWLEDGE_SCHEMA`]).
//! The actual SQLite store implementations live in
//! [`crate::knowledge::store`], [`crate::fact_store`], [`crate::store`] and
//! [`crate::character`]; keeping the DDL here separates the *shape* of the data
//! from the *access* logic.
//!
//! It also holds the persistence utilities those stores share — the unique
//! index installer and the JSON column serializers — so a rule about how data
//! hits the file exists once rather than per store.
//!
//! The module is called `persistence` and not `storage` because it is **not a
//! store**: it has no query API and owns no rows. `storage` sat one letter away
//! from [`crate::store`] (the memory repository) while meaning the opposite
//! thing, which is exactly the kind of name a reader gets wrong.

pub(crate) mod json;
pub mod schema;
pub(crate) mod unique_index;

pub use schema::KNOWLEDGE_SCHEMA;
pub use schema::WORLD_SCHEMA;
