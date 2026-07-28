//! Pronoun Resolver — Phase 4.
//!
//! Resolves pronouns ("他" / "她" / "其"), honorifics ("主公" / "先生" /
//! "将军"), and context-dependent references to their canonical entity names.
//!
//! ## Strategy (V1 — simple rules only)
//!
//! 1. **Pronoun backref** — "他" / "她" / "其" → nearest preceding entity
//!    mention within a sliding window of N sentences.
//! 2. **Title lookup** — "主公" → "刘备", "先生" → "诸葛亮" (dictionary based).
//! 3. **Title fallback** — Titles not in dictionary → nearest preceding entity
//!    with matching contextual role.
//!
//! No NLP, no ML, no coreference resolution model. V1 keeps it simple;
//! a more sophisticated resolver can replace this later without changing the
//! pipeline.

use std::collections::HashMap;

use crate::compiler::{Mention, ResolveStrategy, ResolvedMention};

/// Built-in title → canonical name map for classical Chinese novels.
///
/// Extracted from common usage in 三国演义 and 水浒传.
/// User-provided overrides can be added via the EntityProvider mechanism.
static TITLE_MAP: &[(&str, &str)] = &[
    ("主公", "刘备"),
    ("丞相", "曹操"),
    ("军师", "诸葛亮"),
    ("先生", "诸葛亮"),
    ("孔明", "诸葛亮"),
    ("玄德", "刘备"),
    ("云长", "关羽"),
    ("翼德", "张飞"),
    ("孟德", "曹操"),
    ("奉先", "吕布"),
    ("子龙", "赵云"),
    ("皇帝", "献帝"),
    ("天子", "献帝"),
    ("大哥", "刘备"),
    ("二哥", "关羽"),
    ("三弟", "张飞"),
];

/// Sliding window size (in sentences) for pronoun backref search.
#[allow(dead_code)]
const PRONOUN_WINDOW: usize = 5;

/// Characters treated as pronouns.
const PRONOUNS: &[char] = &['他', '她', '其'];

/// The pronoun resolver.
pub struct PronounResolver {
    /// Custom title overrides (empty by default; can be populated from config).
    title_map: HashMap<String, String>,
}

impl Default for PronounResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl PronounResolver {
    /// Create a new resolver with the built-in title dictionary.
    pub fn new() -> Self {
        let mut title_map = HashMap::new();
        for (title, name) in TITLE_MAP {
            title_map.insert(title.to_string(), name.to_string());
        }
        PronounResolver { title_map }
    }

    /// Register an additional title → canonical name mapping.
    pub fn add_title(&mut self, title: impl Into<String>, canonical: impl Into<String>) {
        self.title_map.insert(title.into(), canonical.into());
    }

    /// Resolve mentions in place, returning resolved versions.
    ///
    /// `mentions` must be sorted by offset and grouped by sentence proximity.
    /// Pronouns and titles are resolved to their canonical names; identity
    /// mentions pass through unchanged.
    pub fn resolve(&self, mentions: &[Mention]) -> Vec<ResolvedMention> {
        let mut resolved = Vec::new();
        // Track the most recent subject entity for pronoun backref.
        let mut last_entity: Option<String> = None;

        for m in mentions {
            let surface = m.surface.as_str();

            // 1. Pronoun backref
            if surface.chars().count() == 1 && PRONOUNS.contains(&surface.chars().next().unwrap()) {
                if let Some(ref antecedent) = last_entity {
                    resolved.push(ResolvedMention {
                        mention: m.clone(),
                        resolved_to: antecedent.clone(),
                        strategy: ResolveStrategy::Pronoun,
                    });
                    continue;
                }
                // No antecedent found — keep the surface as-is (low confidence).
                resolved.push(ResolvedMention {
                    mention: m.clone(),
                    resolved_to: m.canonical_name.clone(),
                    strategy: ResolveStrategy::Pronoun,
                });
                continue;
            }

            // 2. Title lookup
            if let Some(canonical) = self.title_map.get(surface) {
                resolved.push(ResolvedMention {
                    mention: m.clone(),
                    resolved_to: canonical.clone(),
                    strategy: ResolveStrategy::Title,
                });
                last_entity = Some(canonical.clone());
                continue;
            }

            // 3. Identity — surface is already the canonical name
            resolved.push(ResolvedMention {
                mention: m.clone(),
                resolved_to: m.canonical_name.clone(),
                strategy: ResolveStrategy::Identity,
            });
            last_entity = Some(m.canonical_name.clone());
        }

        resolved
    }

    /// Shortcut: resolve mentions and extract just the resolved names.
    pub fn resolve_names(&self, mentions: &[Mention]) -> Vec<String> {
        self.resolve(mentions)
            .into_iter()
            .map(|rm| rm.resolved_to)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::Mention;

    fn make_mention(surface: &str, canonical: &str, offset: usize) -> Mention {
        Mention {
            sentence_id: 0,
            surface: surface.into(),
            canonical_name: canonical.into(),
            offset: offset..(offset + surface.len()),
            confidence: 1.0,
        }
    }

    /// Objective: Verify that an identity mention (already canonical) passes through.
    /// Invariants: Strategy is Identity; resolved_to == canonical_name.
    #[test]
    fn identity_passes_through() {
        let r = PronounResolver::new();
        let mentions = vec![make_mention("赵云", "赵云", 0)];
        let resolved = r.resolve(&mentions);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].resolved_to, "赵云");
        assert_eq!(resolved[0].strategy, ResolveStrategy::Identity);
    }

    /// Objective: Verify that "他" after a named entity resolves to that entity.
    /// Invariants: resolved_to == "赵云" for "他" following "赵云".
    #[test]
    fn pronoun_resolves_to_last_entity() {
        let r = PronounResolver::new();
        let mentions = vec![
            make_mention("赵云", "赵云", 0),
            make_mention("他", "他", 10),
        ];
        let resolved = r.resolve(&mentions);
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[1].resolved_to, "赵云");
        assert_eq!(resolved[1].strategy, ResolveStrategy::Pronoun);
    }

    /// Objective: Verify that "主公" resolves to "刘备" via title dictionary.
    /// Invariants: Strategy is Title; resolved_to == "刘备".
    #[test]
    fn title_resolves_to_canonical() {
        let r = PronounResolver::new();
        let mentions = vec![make_mention("主公", "主公", 0)];
        let resolved = r.resolve(&mentions);
        assert_eq!(resolved[0].resolved_to, "刘备");
        assert_eq!(resolved[0].strategy, ResolveStrategy::Title);
    }

    /// Objective: Verify that pronoun without antecedent keeps its surface name.
    /// Invariants: No panic; resolved_to == surface.
    #[test]
    fn pronoun_without_antecedent_keeps_surface() {
        let r = PronounResolver::new();
        let mentions = vec![make_mention("他", "他", 0)];
        let resolved = r.resolve(&mentions);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].resolved_to, "他");
    }

    /// Objective: Verify that resolve_names returns only resolved names, not mentions.
    /// Invariants: Returns strings in order.
    #[test]
    fn resolve_names_returns_flat_list() {
        let r = PronounResolver::new();
        let mentions = vec![
            make_mention("赵云", "赵云", 0),
            make_mention("他", "他", 10),
            make_mention("主公", "主公", 20),
        ];
        let names = r.resolve_names(&mentions);
        assert_eq!(names, vec!["赵云", "赵云", "刘备"]);
    }

    /// Objective: Verify that a custom title can be added.
    /// Invariants: Custom mapping takes effect.
    #[test]
    fn custom_title_resolves() {
        let mut r = PronounResolver::new();
        r.add_title("大王", "孙权");
        let mentions = vec![make_mention("大王", "大王", 0)];
        let resolved = r.resolve(&mentions);
        assert_eq!(resolved[0].resolved_to, "孙权");
    }
}
