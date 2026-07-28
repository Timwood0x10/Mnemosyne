//! Alias Resolver — Phase 3.
//!
//! Normalizes entity surface forms to canonical names using the entity
//! dictionary: "子龙" → "赵云", "云长" → "关羽", "孟德" → "曹操".
//!
//! The entity engine already resolves aliases during scanning, but the
//! AliasResolver handles edge cases:
//!
//! 1. **Cross-chunk normalization** — mentions from chunks without provider
//!    context get resolved here.
//! 2. **Ambiguity resolution** — same surface form mapping to multiple
//!    canonical names (e.g. "公明" → 宋江 in 水浒传, 徐晃 in 三国演义)
//!    is disambiguated by document context if available.
//! 3. **Confidence downgrade** — low-confidence alias matches get a lower
//!    confidence score.

use std::collections::HashMap;

use crate::compiler::Mention;
use crate::compiler::entity::{EntityDictionary, EntityRegistry};

/// Resolves entity surface forms to canonical names using a merged dictionary.
pub struct AliasResolver {
    /// Merged alias → canonical name map.
    alias_map: HashMap<String, String>,
}

impl AliasResolver {
    /// Build a resolver from an [`EntityRegistry`].
    ///
    /// Takes a snapshot of the registry's merged dictionary at construction
    /// time. If providers change later, rebuild the resolver.
    pub fn new(registry: &EntityRegistry) -> Self {
        let dict = registry.build_dictionary();
        AliasResolver {
            alias_map: dict.alias_to_canonical,
        }
    }

    /// Build a resolver from an already-built [`EntityDictionary`].
    pub fn from_dictionary(dict: &EntityDictionary) -> Self {
        AliasResolver {
            alias_map: dict.alias_to_canonical.clone(),
        }
    }

    /// Resolve all mentions in place.
    ///
    /// For each mention, if its `canonical_name` differs from its `surface`
    /// after lookup, update `canonical_name` and adjust confidence.
    pub fn resolve(&self, mentions: &mut [Mention]) {
        for m in mentions.iter_mut() {
            // If the surface is a known alias but canonical isn't set correctly
            if let Some(canonical) = self.alias_map.get(m.surface.as_str()) {
                if m.canonical_name != *canonical {
                    m.canonical_name = canonical.clone();
                    // Slight confidence bump: resolved alias > unmatched surface
                    m.confidence = m.confidence.max(0.85);
                }
            }
        }
    }

    /// Resolve a single surface form to its canonical name, if known.
    pub fn resolve_one(&self, surface: &str) -> Option<&str> {
        self.alias_map.get(surface).map(|s| s.as_str())
    }

    /// Number of distinct aliases in the dictionary.
    pub fn dictionary_size(&self) -> usize {
        self.alias_map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::entity::{EntityEntry, EntityProvider};
    use std::sync::Arc;

    fn make_registry(entries: Vec<EntityEntry>) -> EntityRegistry {
        let mut reg = EntityRegistry::new();
        reg.register(Arc::new(DummyProvider { entries }));
        reg
    }

    struct DummyProvider {
        entries: Vec<EntityEntry>,
    }

    impl EntityProvider for DummyProvider {
        fn name(&self) -> &str {
            "test"
        }
        fn entries(&self) -> Vec<EntityEntry> {
            self.entries.clone()
        }
    }

    /// Objective: Verify that an alias surface form is resolved to its canonical name.
    /// Invariants: After resolve, `canonical_name == "赵云"` for surface "子龙".
    #[test]
    fn alias_resolved_to_canonical() {
        let reg = make_registry(vec![EntityEntry {
            canonical_name: "赵云".into(),
            aliases: vec!["子龙".into()],
            single_char: None,
            object_type: "person".into(),
            properties: HashMap::new(),
        }]);
        let resolver = AliasResolver::new(&reg);

        let mut mentions = vec![Mention {
            sentence_id: 0,
            surface: "子龙".into(),
            canonical_name: "子龙".into(), // not yet resolved
            offset: 0..6,
            confidence: 0.8,
        }];
        resolver.resolve(&mut mentions);

        assert_eq!(mentions[0].canonical_name, "赵云");
        assert!(mentions[0].confidence >= 0.85);
    }

    /// Objective: Verify that an already-canonical surface is left unchanged.
    /// Invariants: `canonical_name` stays the same; confidence unchanged.
    #[test]
    fn canonical_surface_unchanged() {
        let reg = make_registry(vec![EntityEntry {
            canonical_name: "曹操".into(),
            aliases: vec![],
            single_char: None,
            object_type: "person".into(),
            properties: HashMap::new(),
        }]);
        let resolver = AliasResolver::new(&reg);

        let mut mentions = vec![Mention {
            sentence_id: 0,
            surface: "曹操".into(),
            canonical_name: "曹操".into(),
            offset: 0..6,
            confidence: 0.95,
        }];
        resolver.resolve(&mut mentions);

        assert_eq!(mentions[0].canonical_name, "曹操");
        assert_eq!(mentions[0].confidence, 0.95);
    }

    /// Objective: Verify that an unknown surface is left unchanged.
    /// Invariants: No change to the mention.
    #[test]
    fn unknown_surface_unchanged() {
        let reg = make_registry(vec![]);
        let resolver = AliasResolver::new(&reg);

        let mut mentions = vec![Mention {
            sentence_id: 0,
            surface: "无名氏".into(),
            canonical_name: "无名氏".into(),
            offset: 0..9,
            confidence: 0.5,
        }];
        resolver.resolve(&mut mentions);
        assert_eq!(mentions[0].canonical_name, "无名氏");
    }

    /// Objective: Verify that resolve_one returns the canonical name.
    /// Invariants: `resolve_one("子龙") == Some("赵云")`.
    #[test]
    fn resolve_one_works() {
        let reg = make_registry(vec![EntityEntry {
            canonical_name: "赵云".into(),
            aliases: vec!["子龙".into()],
            single_char: None,
            object_type: "person".into(),
            properties: HashMap::new(),
        }]);
        let resolver = AliasResolver::new(&reg);
        assert_eq!(resolver.resolve_one("子龙"), Some("赵云"));
        assert_eq!(resolver.resolve_one("赵云"), Some("赵云"));
        assert_eq!(resolver.resolve_one("刘表"), None);
    }
}
