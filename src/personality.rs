//! Personality Profile Extractor — extracts structured personality traits
//! from evidence text using keyword patterns.
//!
//! This is a lightweight rule-based extractor (no ML, no LLM). It scans text
//! for personality-relevant phrases and maps them to structured traits.
//!
//! ## Usage
//!
//! ```ignore
//! let traits = extract_personality(text);
//! // → [Trait { name: "sensitive", confidence: 0.85, evidence: "心重些" }, ...]
//! ```

use crate::knowledge::Evidence;

/// A single personality trait with confidence and supporting evidence.
#[derive(Debug, Clone)]
pub struct Trait {
    /// Normalized trait name (e.g. "sensitive", "witty", "melancholic").
    pub name: String,
    /// Confidence score [0.0, 1.0].
    pub confidence: f64,
    /// The text snippet that supports this trait.
    pub evidence: String,
}

/// A complete personality profile for an entity.
#[derive(Debug, Clone)]
pub struct PersonalityProfile {
    /// Entity name.
    pub entity: String,
    /// Extracted traits, sorted by descending confidence.
    pub traits: Vec<Trait>,
    /// Free-text summary (empty for V1).
    pub summary: String,
}

/// Personality keyword patterns: (keyword, trait_name, weight).
const TRAIT_PATTERNS: &[(&str, &str, f64)] = &[
    // 黛玉-specific
    ("心重", "sensitive", 0.85),
    ("多心", "suspicious", 0.80),
    ("善妒", "jealous", 0.85),
    ("爱哭", "tearful", 0.80),
    ("哭", "emotional", 0.50),
    ("笑", "cheerful", 0.30),
    ("葬花", "melancholic", 0.90),
    ("焚稿", "despairing", 0.95),
    ("焚稿断痴情", "despairing", 0.95),
    ("断痴情", "despairing", 0.90),
    ("俏语", "witty", 0.85),
    ("谑娇音", "playful", 0.80),
    ("诗", "poetic", 0.60),
    ("吟诗", "poetic", 0.70),
    ("多愁", "melancholic", 0.85),
    ("善感", "sensitive", 0.80),
    ("多病", "frail", 0.75),
    ("体弱", "frail", 0.70),
    ("咳嗽", "frail", 0.50),
    ("嗽疾", "frail", 0.70),
    ("孤傲", "proud", 0.85),
    ("清高", "proud", 0.80),
    ("聪明", "intelligent", 0.85),
    ("聪慧", "intelligent", 0.85),
    ("才华", "talented", 0.75),
    ("才情", "talented", 0.80),
    // General personality descriptors
    ("温柔", "gentle", 0.70),
    ("刚烈", "fierce", 0.85),
    ("宽厚", "generous", 0.70),
    ("仁慈", "kind", 0.70),
    ("多疑", "suspicious", 0.75),
    ("性急", "impatient", 0.70),
    ("急躁", "impatient", 0.70),
    ("勇猛", "brave", 0.70),
    ("奸雄", "cunning", 0.80),
    ("枭雄", "ambitious", 0.75),
    ("狡诈", "deceitful", 0.80),
];

/// Extract a personality profile from evidence snippets.
///
/// Scans each evidence text for known personality patterns and aggregates
/// the results into a profile sorted by descending confidence.
pub fn extract_profile(entity: &str, evidence: &[Evidence]) -> PersonalityProfile {
    let mut trait_scores: std::collections::HashMap<String, (f64, String)> =
        std::collections::HashMap::new();

    for ev in evidence {
        let text = &ev.content;
        for &(keyword, trait_name, weight) in TRAIT_PATTERNS {
            if text.contains(keyword) {
                let entry = trait_scores
                    .entry(trait_name.to_string())
                    .or_insert_with(|| (0.0, String::new()));
                entry.0 = entry.0.max(weight);
                // Keep the shortest evidence snippet that matches
                if entry.1.is_empty() || text.len() < entry.1.len() {
                    let snippet: String = text.chars().take(80).collect();
                    entry.1 = snippet;
                }
            }
        }
    }

    let mut traits: Vec<Trait> = trait_scores
        .into_iter()
        .map(|(name, (confidence, evidence))| Trait {
            name,
            confidence,
            evidence,
        })
        .collect();

    traits.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    PersonalityProfile {
        entity: entity.to_string(),
        traits,
        summary: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify that known personality keywords are detected.
    /// Invariants: "心重" should produce a "sensitive" trait.
    #[test]
    fn detects_sensitive() {
        let evidence = vec![Evidence {
            id: 1,
            doc_id: 1,
            chapter_id: 1,
            content: "林丫头那孩子倒罢了，只是心重些".into(),
            start_offset: None,
            end_offset: None,
            created_at: 0,
        }];
        let profile = extract_profile("林黛玉", &evidence);
        assert!(
            profile.traits.iter().any(|t| t.name == "sensitive"),
            "should detect 'sensitive' from '心重'"
        );
    }

    /// Objective: Verify that multiple evidence items for the same trait
    /// produce the maximum confidence score.
    /// Invariants: "葬花" (0.90) and "多愁" (0.85) → trait "melancholic" at 0.90.
    #[test]
    fn max_confidence_used() {
        let evidence = vec![
            Evidence {
                id: 1,
                doc_id: 1,
                chapter_id: 1,
                content: "侬今葬花人笑痴".into(),
                start_offset: None,
                end_offset: None,
                created_at: 0,
            },
            Evidence {
                id: 2,
                doc_id: 1,
                chapter_id: 1,
                content: "多愁善感".into(),
                start_offset: None,
                end_offset: None,
                created_at: 0,
            },
        ];
        let profile = extract_profile("林黛玉", &evidence);
        let mel = profile
            .traits
            .iter()
            .find(|t| t.name == "melancholic")
            .unwrap();
        assert!(
            (mel.confidence - 0.90).abs() < 0.01,
            "max confidence should be 0.90 (from '葬花')"
        );
    }

    /// Objective: Verify that empty evidence returns an empty profile.
    /// Invariants: No evidence → empty traits list.
    #[test]
    fn empty_evidence_returns_empty() {
        let profile = extract_profile("林黛玉", &[]);
        assert!(
            profile.traits.is_empty(),
            "no evidence should produce no traits"
        );
    }
}
