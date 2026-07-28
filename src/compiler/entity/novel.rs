//! Novel provider — hardcoded entity definitions for the Four Great Classical
//! Novels. Ported from `ingest/characters.rs`.
//!
//! TODO: port the static character tables from `ingest/characters.rs` during Phase 3.

use super::provider::{EntityEntry, EntityProvider};

pub struct NovelProvider;

impl EntityProvider for NovelProvider {
    fn name(&self) -> &str {
        "novel"
    }

    fn entries(&self) -> Vec<EntityEntry> {
        // Phase 3: port from ingest/characters.rs
        Vec::new()
    }
}
