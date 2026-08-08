//! Domain profile packs — pluggable extraction rules per domain.
//!
//! The generalization plan's step 2: extraction rules must not be hardcoded
//! for classical Chinese novels. Each domain ships a JSON "profile pack" that
//! declares which entity attributes it can extract, which relation
//! predicates it recognizes, and which verb/hint groups drive classification
//! (positive/negative/goal/decision…). The lexicon (`lexicon/packs/*.json`)
//! supplies vocabulary; this module supplies the extraction *shape*.
//!
//! Packs live in `config/domain_profiles/*.json` (env override supported) and
//! are cached once per process via [`std::sync::LazyLock`], mirroring the
//! validator/anchor-seed loading pattern.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Deserialize;

use crate::error::{Error, Result};

/// JSON shape of a domain profile pack.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DomainProfile {
    /// Stable profile name, e.g. `"conversation_cognition"`.
    pub profile_name: String,
    /// Hint for which source kinds this pack applies to (dialog/text/pdf…).
    pub doc_type_hint: String,
    /// Languages the pack's keywords target.
    pub languages: Vec<String>,
    /// Entity attributes this domain can extract.
    pub entity_attrs: Vec<String>,
    /// Relation predicates this domain recognizes.
    pub relation_predicates: Vec<String>,
    /// Verb groups: label → verbs (positive/negative/goal/decision…).
    pub verb_groups: HashMap<String, Vec<String>>,
    /// Extraction hints: label → keywords for classification.
    pub extraction_hints: HashMap<String, Vec<String>>,
}

impl DomainProfile {
    /// Load a named pack from `config/domain_profiles/{name}.json`.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] when the file is missing.
    /// - [`Error::Config`] when the JSON is unparseable.
    pub fn load(name: &str) -> Result<Self> {
        let base = std::env::var("DOMAIN_PROFILES_PATH")
            .unwrap_or_else(|_| "config/domain_profiles".to_string());
        let path = format!("{base}/{name}.json");
        let raw = std::fs::read_to_string(&path).map_err(Error::Io)?;
        serde_json::from_str(&raw).map_err(|e| Error::Config(format!("parse {path}: {e}")))
    }
}

/// Cached conversation-cognition pack (the default domain for dialog input).
static CONVERSATION_PROFILE: LazyLock<DomainProfile> = LazyLock::new(|| {
    // Fail soft instead of panicking at first use: a missing/corrupt
    // `config/domain_profiles/conversation_cognition.json` used to abort the
    // process. Degrade to an EMPTY profile (keyword extraction simply misses)
    // and log the cause so a deployment without the config stays alive.
    match DomainProfile::load("conversation_cognition") {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "warning: conversation_cognition profile failed to load ({e}); using an empty profile"
            );
            DomainProfile::default()
        }
    }
});

/// Access the process-cached conversation-cognition profile.
#[must_use]
pub fn conversation_profile() -> &'static DomainProfile {
    &CONVERSATION_PROFILE
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify the conversation profile pack parses with the
    /// expected domain shape.
    /// Invariants: profile_name matches; entity_attrs includes preference
    /// and goal; verb_groups has positive/negative; extraction_hints has
    /// decision_keywords.
    #[test]
    fn conversation_pack_parses() {
        let p = DomainProfile::load("conversation_cognition").expect("pack must load");
        assert_eq!(p.profile_name, "conversation_cognition");
        assert!(p.entity_attrs.iter().any(|a| a == "preference"));
        assert!(p.entity_attrs.iter().any(|a| a == "goal"));
        assert!(p.verb_groups.contains_key("positive"));
        assert!(p.verb_groups.contains_key("negative"));
        assert!(
            p.extraction_hints
                .get("decision_keywords")
                .is_some_and(|kws| kws.iter().any(|k| k == "决定")),
            "decision keywords must include 决定"
        );
        assert!(p.relation_predicates.iter().any(|r| r == "喜欢"));
    }

    /// Objective: Verify a missing pack surfaces an Io error (never panic).
    /// Invariants: nonexistent name → Err(Io).
    #[test]
    fn missing_pack_errors() {
        let r = DomainProfile::load("does_not_exist_xyz");
        assert!(matches!(r, Err(Error::Io(_))), "missing pack → Io error");
    }

    /// Objective: Verify the cached conversation profile is accessible and
    /// consistent with a fresh load.
    /// Invariants: cached profile_name equals fresh-load profile_name.
    #[test]
    fn cached_profile_matches_fresh_load() {
        let cached = conversation_profile();
        let fresh = DomainProfile::load("conversation_cognition").expect("fresh");
        assert_eq!(cached.profile_name, fresh.profile_name);
        assert_eq!(cached.languages, fresh.languages);
    }
}
