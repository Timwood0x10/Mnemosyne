//! Entity dictionary — maps aliases to canonical names and entity IDs.
//!
//! Populated during Pass 1 (World Builder) and consulted in Pass 2 (Story
//! Compiler) to resolve mentions.
//!
//! ## Lookup path
//!
//! ```text
//! "玄德" → AliasIndex → "刘备" → name_to_id → 10001
//! ```

use std::collections::HashMap;

use super::provider::EntityEntry;

/// Merged entity index — alias → (canonical_name, entity_id).
#[derive(Debug, Default)]
pub struct EntityDictionary {
    pub entries: Vec<EntityEntry>,
    /// Alias (including canonical name itself) → canonical name.
    pub alias_to_canonical: HashMap<String, String>,
    /// Canonical name → entity_id (assigned by Pass 1).
    pub name_to_id: HashMap<String, i64>,
}

impl EntityDictionary {
    /// Register a discovered entity at runtime (from Pass 1 auto-discovery).
    ///
    /// This adds the canonical name and all aliases to the dictionary so that
    /// Pass 2 (Story Compiler) can resolve mentions.
    pub fn register_discovered(&mut self, name: &str, aliases: &[&str]) {
        self.alias_to_canonical
            .insert(name.to_string(), name.to_string());
        for alias in aliases {
            self.alias_to_canonical
                .insert(alias.to_string(), name.to_string());
        }
    }
}

impl EntityDictionary {
    /// Assign entity IDs after Pass 1 creates the Entity nodes.
    ///
    /// Called after [`EntityStore`](crate::knowledge::store::KnowledgeStore)
    /// returns the auto-generated IDs.
    pub fn assign_ids(&mut self, name_to_id: HashMap<String, i64>) {
        self.name_to_id = name_to_id;
    }

    /// Look up an alias and return (canonical_name, entity_id) if known.
    pub fn resolve(&self, alias: &str) -> Option<(String, Option<i64>)> {
        let canonical = self.alias_to_canonical.get(alias)?;
        let id = self.name_to_id.get(canonical).copied();
        Some((canonical.clone(), id))
    }

    /// Look up an alias and return just the canonical name.
    pub fn resolve_name(&self, alias: &str) -> Option<&str> {
        self.alias_to_canonical.get(alias).map(|s| s.as_str())
    }
}
