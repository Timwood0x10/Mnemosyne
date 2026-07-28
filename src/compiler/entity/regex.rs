//! Regex provider — lets users specify custom entity patterns via CLI flags
//! or config. Each pattern maps a regex match to a named entity.
//!
//! Example:
//! ```bash
//! --entity "Rust::language" --entity "Tokio::library"
//! ```
//!
//! TODO: implement during Phase 3.

use super::provider::{EntityEntry, EntityProvider};

pub struct RegexProvider;

impl EntityProvider for RegexProvider {
    fn name(&self) -> &str {
        "regex"
    }

    fn entries(&self) -> Vec<EntityEntry> {
        // Phase 3: parse user-supplied regex → entity mappings
        Vec::new()
    }
}
