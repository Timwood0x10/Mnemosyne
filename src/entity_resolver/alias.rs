//! Alias Resolver — Stage 1 of the Entity Resolution Pipeline.
//!
//! Performs exact-match resolution using a pre-built HashMap of entity aliases.
//! Mention text is normalized (trim + fullwidth→halfwidth + whitespace stripped)
//! before lookup, so "刘 皇叔" and "刘皇叔" both resolve to the same entity.

use std::collections::HashMap;

use crate::entity_resolver::pipeline::ResolverStage;
use crate::entity_resolver::{ResolveContext, ResolveResult};

/// Resolves mentions via exact alias lookup.
pub struct AliasResolver {
    /// Normalized alias → entity_id.
    map: HashMap<String, i64>,
}

impl AliasResolver {
    /// Build an empty resolver (for testing or incremental construction).
    pub fn empty() -> Self {
        AliasResolver {
            map: HashMap::new(),
        }
    }

    /// Build a resolver from an iterator of (alias, entity_id) pairs.
    ///
    /// Each alias is normalized before insertion. Named `from_pairs` rather
    /// than `from_iter` to avoid shadowing `std::iter::FromIterator::from_iter`
    /// (clippy::should_implement_trait).
    pub fn from_pairs(iter: impl IntoIterator<Item = (String, i64)>) -> Self {
        let mut map = HashMap::new();
        for (alias, id) in iter {
            let key = normalize(&alias);
            if !key.is_empty() {
                map.insert(key, id);
            }
        }
        AliasResolver { map }
    }

    /// Add a single alias → entity_id mapping.
    pub fn insert(&mut self, alias: &str, entity_id: i64) {
        let key = normalize(alias);
        if !key.is_empty() {
            self.map.insert(key, entity_id);
        }
    }

    /// Resolve a mention to an entity ID by exact alias match.
    ///
    /// Returns `Some(entity_id)` if the normalized mention exists in the map,
    /// or `None` if it does not (in which case the next pipeline stage should
    /// attempt fuzzy resolution).
    pub fn resolve(&self, mention: &str) -> Option<i64> {
        let key = normalize(mention);
        self.map.get(&key).copied()
    }

    /// Number of entries in the alias map.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Returns `true` if the alias map is empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// A resolver stage that performs exact alias matching.
///
/// Wraps an [`AliasResolver`] and implements [`ResolverStage`] so it can be
/// plugged into a [`ResolverPipeline`](crate::entity_resolver::ResolverPipeline).
pub struct AliasStage {
    resolver: AliasResolver,
}

impl AliasStage {
    /// Create a new alias stage with the given resolver.
    pub fn new(resolver: AliasResolver) -> Self {
        AliasStage { resolver }
    }
}

impl ResolverStage for AliasStage {
    fn resolve(&self, mention: &str, _ctx: &ResolveContext) -> Option<ResolveResult> {
        self.resolver
            .resolve(mention)
            .map(|entity_id| ResolveResult::Matched {
                entity_id,
                score: 1.0,
            })
    }
}

/// Normalize a mention string for consistent HashMap lookup.
///
/// Operations:
/// 1. Strip leading/trailing whitespace.
/// 2. Convert fullwidth ASCII (Ｆ, ｆ, ０) to halfwidth (F, f, 0).
/// 3. Remove all internal whitespace.
fn normalize(s: &str) -> String {
    s.trim()
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| {
            // Fullwidth ASCII (！..～, U+FF01..U+FF5E) → halfwidth via
            // subtracting 0xFEE0. Covers uppercase Ａ..Ｚ, lowercase ａ..ｚ,
            // and digits ０..９ in a single branch. The previous form split
            // these into three `else if` arms with identical bodies, and
            // started the letter ranges at Ｆ/ｆ — silently skipping
            // Ａ..Ｅ and ａ..ｅ (clippy::if_same_then_else surfaced both the
            // duplication and the missing ranges).
            if ('！'..='～').contains(&c) {
                char::from_u32(c as u32 - 0xFEE0).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify that exact alias lookup returns the correct entity ID.
    /// Invariants: "玄德" → 10001 (刘备).
    #[test]
    fn exact_alias_resolves_correctly() {
        let resolver = AliasResolver::from_pairs([("玄德".into(), 10001)]);
        let result = resolver.resolve("玄德");
        assert_eq!(
            result,
            Some(10001),
            "alias '玄德' should resolve to entity 10001"
        );
    }

    /// Objective: Verify that an unknown mention returns None.
    /// Invariants: No false positives for unrecognized input.
    #[test]
    fn unknown_mention_returns_none() {
        let resolver = AliasResolver::empty();
        let result = resolver.resolve("朴素的");
        assert_eq!(result, None, "unknown mention should return None");
    }

    /// Objective: Verify that whitespace differences do not affect resolution.
    /// Invariants: "刘皇叔", " 刘皇叔 ", "刘 皇叔" all resolve to the same ID.
    #[test]
    fn normalize_removes_whitespace() {
        let mut resolver = AliasResolver::empty();
        resolver.insert("刘皇叔", 10001);

        assert_eq!(resolver.resolve("刘皇叔"), Some(10001), "no whitespace");
        assert_eq!(
            resolver.resolve(" 刘皇叔 "),
            Some(10001),
            "outer whitespace"
        );
        assert_eq!(
            resolver.resolve("刘  皇叔"),
            Some(10001),
            "inner whitespace"
        );
    }

    /// Objective: Verify that fullwidth characters are normalized to halfwidth.
    /// Invariants: "曹操" and fullwidth variant resolve to the same ID.
    #[test]
    fn normalize_fullwidth_ascii() {
        let mut resolver = AliasResolver::empty();
        resolver.insert("曹操", 10002);

        // Fullwidth equivalent: ＣＡＯ ＣＡＯ is not meaningful Chinese,
        // but test numeric fullwidth in an alias like "第１２３回"
        resolver.insert("第123回", 999);
        assert_eq!(
            resolver.resolve("第１２３回"),
            Some(999),
            "fullwidth digits"
        );
        assert_eq!(resolver.resolve("第123回"), Some(999), "halfwidth digits");
    }

    /// Objective: Verify that empty mentions produce no match.
    /// Invariants: Empty string after normalization does not create an entry.
    #[test]
    fn empty_mention_no_match() {
        let mut resolver = AliasResolver::empty();
        resolver.insert("  ", 42);
        assert!(
            resolver.is_empty(),
            "whitespace-only alias should not be stored"
        );
        assert_eq!(
            resolver.resolve(""),
            None,
            "empty string should return None"
        );
    }

    /// Objective: Verify `from_pairs` rejects empty keys.
    /// Invariants: Only non-empty normalized keys are inserted.
    #[test]
    fn from_pairs_filters_empty() {
        let resolver = AliasResolver::from_pairs([("".into(), 1), ("曹操".into(), 2)]);
        assert_eq!(resolver.len(), 1, "only one alias should be stored");
        assert_eq!(resolver.resolve("曹操"), Some(2));
    }
}
