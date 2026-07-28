//! Entity registry — merges multiple [`EntityProvider`]s into a single
//! dictionary and runs the matching engine.
//!
//! ## Matching priority
//!
//! 1. Full alias match (longest-first via Trie)
//! 2. Single-character match (safe context only)
//! 3. Aho-Corasick for bulk keywords (optional)

use std::collections::HashMap;
use std::sync::Arc;

use super::provider::{EntityEntry, EntityProvider};

/// Merged entity dictionary from all registered providers.
#[derive(Debug, Default)]
pub struct EntityDictionary {
    pub entries: Vec<EntityEntry>,
    pub alias_to_canonical: HashMap<String, String>,
}

/// Entity registry that accepts multiple providers and produces a dictionary.
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

impl Default for EntityRegistry {
    fn default() -> Self {
        Self { providers: Vec::new() }
    }
}

impl EntityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, provider: Arc<dyn EntityProvider>) {
        self.providers.push(provider);
    }

    /// Merge all registered providers into a single dictionary.
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
        }
    }
}
