//! Entity registry — maps aliases to entity IDs and canonical names.
//!
//! Built from one or more [`EntityProvider`]s. The registry is populated
//! during Pass 1 (World Builder) and used in Pass 2 (Story Compiler) to
//! resolve mentions to entity IDs.
//!
//! ## Lookup path
//!
//! ```text
//! "玄德" → AliasIndex → "刘备" → EntityIdIndex → 10001
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use super::provider::{EntityEntry, EntityProvider};

/// Merged entity index — alias → (canonical_name, entity_id).
#[derive(Debug, Default)]
pub struct EntityDictionary {
    pub entries: Vec<EntityEntry>,
    /// Alias (including canonical name itself) → canonical name.
    pub alias_to_canonical: HashMap<String, String>,
    /// Canonical name → entity_id (assigned by Pass 1).
    pub name_to_id: HashMap<String, i64>,
}

/// Entity registry that accepts multiple providers and builds an index.
#[derive(Default)]
pub struct EntityRegistry {
    providers: Vec<Arc<dyn EntityProvider>>,
}

impl std::fmt::Debug for EntityRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntityRegistry")
            .field("provider_count", &self.providers.len())
            .finish()
    }
}

impl EntityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, provider: Arc<dyn EntityProvider>) {
        self.providers.push(provider);
    }

    /// Merge all registered providers into a single index.
    pub fn build_dictionary(&self) -> EntityDictionary {
        let mut entries = Vec::new();
        let mut alias_map = HashMap::new();

        for provider in &self.providers {
            for entry in provider.entries() {
                alias_map.insert(entry.canonical_name.clone(), entry.canonical_name.clone());
                for alias in &entry.aliases {
                    alias_map.insert(alias.clone(), entry.canonical_name.clone());
                }
                if let Some(sc) = &entry.single_char {
                    alias_map.insert(sc.clone(), entry.canonical_name.clone());
                }
                entries.push(entry);
            }
        }

        EntityDictionary {
            entries,
            alias_to_canonical: alias_map,
            name_to_id: HashMap::new(),
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
