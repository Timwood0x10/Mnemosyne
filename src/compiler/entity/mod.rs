//! Entity Engine — Phase 3.
//!
//! Discovers entity mentions in unstructured text using an [`EntityRegistry`]
//! that merges multiple [`EntityProvider`]s. The engine uses Aho-Corasick for
//! multi-pattern matching against known names and aliases, then supplements
//! with safe single-character shortname matches ("飞曰"→张飞).

mod conversation;
mod corpus;
mod json_provider;
mod novel;
mod provider;
mod regex;
mod registry;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use std::collections::HashMap;

use crate::compiler::{Mention, Sentence};
use crate::ingest::extract::{ACTION_VERBS, DIALOG_VERBS, floor_char_boundary};

pub use conversation::ConversationProvider;
pub use corpus::CorpusEntityProvider;
pub use json_provider::JsonEntityProvider;
pub use novel::NovelProvider;
pub use provider::{EntityEntry, EntityProvider};
pub use regex::RegexProvider;
pub use registry::{EntityDictionary, EntityRegistry};

/// A single-char shortname spec (equivalent to the old
/// [`SingleCharSpec`] from [`ingest::extract`]).
struct ShortSpec {
    short: String,
    name: String,
}

/// The entity engine — caches the merged dictionary and matching automata.
pub struct EntityEngine {
    _registry: EntityRegistry,
    /// Merged alias → (canonical_name, object_type, confidence)
    alias_map: HashMap<String, (String, String, f64)>,
    /// Aho-Corasick automaton built from all known aliases + names.
    ac: AhoCorasick,
    /// Single-char specs for safe shortname matching.
    short_specs: Vec<ShortSpec>,
}

impl EntityEngine {
    /// Build an engine from a pre-configured registry.
    ///
    /// # Panics
    ///
    /// Panics if the registry contains zero entries (the automaton would be
    /// empty). Providers should supply at least one entry.
    pub fn new(registry: EntityRegistry) -> Self {
        let dict = registry.build_dictionary();
        let mut patterns: Vec<String> = Vec::new();
        let mut alias_map: HashMap<String, (String, String, f64)> = HashMap::new();
        let mut short_specs: Vec<ShortSpec> = Vec::new();

        for entry in &dict.entries {
            // Map the canonical name
            alias_map.insert(
                entry.canonical_name.clone(),
                (entry.canonical_name.clone(), entry.object_type.clone(), 1.0),
            );
            patterns.push(entry.canonical_name.clone());

            // Map each alias
            for alias in &entry.aliases {
                alias_map.insert(
                    alias.clone(),
                    (entry.canonical_name.clone(), entry.object_type.clone(), 0.9),
                );
                patterns.push(alias.clone());
            }

            // Single-char spec
            if let Some(sc) = &entry.single_char {
                short_specs.push(ShortSpec {
                    short: sc.clone(),
                    name: entry.canonical_name.clone(),
                });
            }
        }

        let ac = AhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostLongest)
            .build(&patterns)
            .expect("Aho-Corasick automaton must be buildable with non-empty patterns");

        EntityEngine {
            _registry: registry,
            alias_map,
            ac,
            short_specs,
        }
    }

    /// Scan a chunk of text for entity mentions.
    ///
    /// Returns mentions sorted by byte offset.
    pub fn scan(&self, text: &str, _chunk_index: usize) -> Vec<Mention> {
        let mut mentions = Vec::new();

        // 1. Aho-Corasick multi-pattern match (names + aliases)
        for mat in self.ac.find_iter(text) {
            let span = mat.span();
            let matched = &text[span.start..span.end];
            if let Some((canonical, _obj_type, conf)) = self.alias_map.get(matched) {
                mentions.push(Mention {
                    sentence_id: 0, // filled in later
                    entity_id: None,
                    surface: matched.to_owned(),
                    canonical_name: canonical.clone(),
                    offset: (span.start)..(span.end),
                    confidence: *conf,
                });
            }
        }

        // 2. Single-character shortname matches (inline safe-context check)
        for spec in &self.short_specs {
            let short_len = spec.short.len();
            for (pos, _) in text.match_indices(&spec.short) {
                // Must be preceded by non-Chinese char (or string start)
                let prev_ok = if pos == 0 {
                    true
                } else {
                    let boundary = floor_char_boundary(&text[..pos], pos.saturating_sub(1));
                    let prev_char = text[boundary..].chars().next();
                    match prev_char {
                        None => true,
                        Some(c) => !('\u{4E00}'..='\u{9FFF}').contains(&c),
                    }
                };
                if !prev_ok {
                    continue;
                }
                // Must be followed by a dialog or action verb
                let after = &text[pos + short_len..];
                let followed = DIALOG_VERBS.iter().any(|v| after.starts_with(v))
                    || ACTION_VERBS.iter().any(|v| after.starts_with(v));
                if !followed {
                    continue;
                }
                // Avoid duplicate offset (AC may have already matched)
                let already_has = mentions
                    .iter()
                    .any(|m| m.offset.start == pos && m.offset.end == pos + short_len);
                if !already_has {
                    mentions.push(Mention {
                        sentence_id: 0,
                        entity_id: None,
                        surface: spec.short.clone(),
                        canonical_name: spec.name.clone(),
                        offset: pos..(pos + short_len),
                        confidence: 0.7,
                    });
                }
            }
        }

        // 3. Sort by offset
        mentions.sort_by_key(|a| a.offset.start);

        mentions
    }

    /// Scan a slice of sentences, producing mentions with absolute offsets
    /// and provisional sentence IDs.
    pub fn scan_sentences(&self, sentences: &[Sentence]) -> Vec<Mention> {
        let mut all = Vec::new();
        for (sid, sent) in sentences.iter().enumerate() {
            let local = self.scan(&sent.text, sent.chunk_index);
            for mut m in local {
                // Convert chunk-relative offsets to document-level offsets
                let start_abs = sent.start_offset + m.offset.start;
                let end_abs = sent.start_offset + m.offset.end;
                m.sentence_id = sid;
                m.offset = start_abs..end_abs;
                all.push(m);
            }
        }
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// Objective: Verify that a simple name in text is found as a mention.
    /// Invariants: One mention with the canonical name and correct offset.
    #[test]
    fn scan_finds_full_name() {
        let reg = make_registry(vec![EntityEntry {
            canonical_name: "赵云".into(),
            aliases: vec!["子龙".into()],
            single_char: None,
            object_type: "person".into(),
            properties: HashMap::new(),
        }]);
        let engine = EntityEngine::new(reg);
        let mentions = engine.scan("赵云救阿斗", 0);
        assert!(!mentions.is_empty(), "赵云 should be found");
        assert_eq!(mentions[0].canonical_name, "赵云");
    }

    /// Objective: Verify that alias matching resolves to the canonical name.
    /// Invariants: "子龙" → "赵云" in the mention.
    #[test]
    fn scan_resolves_alias() {
        let reg = make_registry(vec![EntityEntry {
            canonical_name: "赵云".into(),
            aliases: vec!["子龙".into()],
            single_char: None,
            object_type: "person".into(),
            properties: HashMap::new(),
        }]);
        let engine = EntityEngine::new(reg);
        let mentions = engine.scan("子龙救阿斗", 0);
        assert!(!mentions.is_empty());
        assert_eq!(mentions[0].canonical_name, "赵云");
        assert_eq!(mentions[0].surface, "子龙");
    }

    /// Objective: Verify that empty text produces zero mentions.
    /// Invariants: No panics; empty result.
    #[test]
    fn scan_empty_text() {
        let reg = make_registry(vec![EntityEntry {
            canonical_name: "赵云".into(),
            aliases: vec![],
            single_char: None,
            object_type: "person".into(),
            properties: HashMap::new(),
        }]);
        let engine = EntityEngine::new(reg);
        let mentions = engine.scan("", 0);
        assert!(mentions.is_empty());
    }
}
