//! Novel provider — entity definitions for the Four Great Classical Novels.
//!
//! References the existing static character tables in
//! [`ingest::characters`] and converts them to [`EntityEntry`] format
//! at runtime. No data duplication.

use std::collections::HashMap;

use crate::ingest::characters::{NOVELS, get_novel_characters};

use super::provider::{EntityEntry, EntityProvider};

/// Provider for a single novel's character definitions.
///
/// # Example
///
/// ```ignore
/// let provider = NovelProvider::new("三国演义");
/// let engine = EntityEngine::new(EntityRegistry::from(vec![Arc::new(provider)]));
/// ```
pub struct NovelProvider {
    novel: String,
}

impl NovelProvider {
    /// Create a provider for a single novel.
    ///
    /// Accepts: "水浒传", "三国演义", "红楼梦", "西游记".
    /// Unknown names produce an empty entry list.
    pub fn new(novel: impl Into<String>) -> Self {
        NovelProvider {
            novel: novel.into(),
        }
    }

    /// Create one provider per novel, useful for bulk compilation.
    pub fn all() -> Vec<Self> {
        NOVELS
            .iter()
            .map(|n| NovelProvider {
                novel: n.to_string(),
            })
            .collect()
    }

    /// The novel name this provider was constructed for.
    pub fn novel_name(&self) -> &str {
        &self.novel
    }
}

impl EntityProvider for NovelProvider {
    fn name(&self) -> &str {
        "novel"
    }

    fn entries(&self) -> Vec<EntityEntry> {
        let cdefs = get_novel_characters(&self.novel);
        cdefs
            .iter()
            .map(|c| {
                let mut properties = HashMap::new();
                properties.insert("novel".to_string(), self.novel.clone());

                EntityEntry {
                    canonical_name: c.name.to_string(),
                    aliases: c.aliases.iter().map(|a| a.to_string()).collect(),
                    single_char: c.single_char.map(|s| s.to_string()),
                    object_type: "person".into(),
                    properties,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify that each novel has at least one character entry.
    /// Invariants: All four classical novels return non-empty entry lists.
    #[test]
    fn all_novels_have_entries() {
        for &novel in NOVELS {
            let provider = NovelProvider::new(novel);
            let entries = provider.entries();
            assert!(
                !entries.is_empty(),
                "{novel} should have at least one character"
            );
        }
    }

    /// Objective: Verify that the first entry has the expected shape.
    /// Invariants: Canonical name is non-empty; object_type is "person".
    #[test]
    fn entry_has_correct_shape() {
        let provider = NovelProvider::new("三国演义");
        let entries = provider.entries();
        assert!(!entries.is_empty());
        assert_eq!(entries[0].object_type, "person");
        assert!(!entries[0].canonical_name.is_empty());
        // Properties should include the novel name
        assert_eq!(
            entries[0].properties.get("novel").map(|s| s.as_str()),
            Some("三国演义")
        );
    }

    /// Objective: Verify that an unknown novel name returns an empty list.
    /// Invariants: Empty vec; no panics.
    #[test]
    fn unknown_novel_returns_empty() {
        let provider = NovelProvider::new("不存在的");
        assert!(provider.entries().is_empty());
    }
}
