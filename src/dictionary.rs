//! Runtime-loaded dictionary for entity-name filtering and language-aware processing.
//!
//! ## Design
//!
//! The dictionary is loaded from `config/dictionary.json` at runtime via a global
//! lazy-static [`Dictionary`] instance. Users can edit the JSON file to add or
//! remove words without recompiling.
//!
//! The JSON uses the **Elite Lexicon** format (see ELITE_LEXICON_PLAN.md):
//! each verb is a structured `Lexeme` with a stable ID, language, lemma,
//! semantic class, cognitive effects, polarity, priority, and match constraints.
//! Stop-name lists live under `stop_names.en` / `stop_names.zh`.
//!
//! The module provides a flat-function API (e.g. [`is_english_stop_name`],
//! [`is_chinese_stop_name`]) that delegates to the global dictionary.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;
use std::sync::RwLock;

use serde::Deserialize;

// ── Lexeme types ────────────────────────────────────────────────────────────

/// Lifecycle stage of a lexeme (ELITE_LEXICON_PLAN §10).
///
/// Promotion path: `Candidate → Experimental → Core`.
/// Retirement path: `Core/Experimental → Deprecated → Disabled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LexemeStatus {
    /// Proposed but not yet validated by regression tests.
    #[default]
    Candidate,
    /// Passing module tests; awaiting fixed-corpus regression.
    Experimental,
    /// Passed fixed-corpus regression; ships in the default core.
    Core,
    /// Scheduled for removal; kept for at least one release.
    Deprecated,
    /// Excluded from matchers entirely.
    Disabled,
}

impl LexemeStatus {
    /// Whether the lexeme participates in matching.
    pub fn is_active(self) -> bool {
        !matches!(self, LexemeStatus::Disabled)
    }

    /// Advance one step along the promotion path.
    pub fn promote(self) -> LexemeStatus {
        match self {
            LexemeStatus::Candidate => LexemeStatus::Experimental,
            LexemeStatus::Experimental => LexemeStatus::Core,
            other => other,
        }
    }

    /// Advance one step along the retirement path.
    pub fn demote(self) -> LexemeStatus {
        match self {
            LexemeStatus::Core | LexemeStatus::Experimental => LexemeStatus::Deprecated,
            LexemeStatus::Deprecated => LexemeStatus::Disabled,
            other => other,
        }
    }
}

/// A single entry in the Elite Lexicon.
#[derive(Debug, Clone, Deserialize)]
pub struct Lexeme {
    pub id: String,
    pub language: String,
    pub lemma: String,
    #[serde(default)]
    pub forms: Vec<String>,
    pub pos: String,
    pub semantic_class: String,
    pub effects: Vec<CognitiveEffect>,
    pub polarity: String,
    pub priority: u16,
    pub constraints: MatchConstraints,
    pub source: LexiconSource,
    #[serde(default)]
    pub status: LexemeStatus,
}

/// What a lexeme produces when matched.
#[derive(Debug, Clone, Deserialize)]
pub struct CognitiveEffect {
    #[serde(rename = "type")]
    pub effect_type: String,
    pub value: String,
}

/// Match-time constraints.
#[derive(Debug, Clone, Deserialize)]
pub struct MatchConstraints {
    #[serde(default)]
    pub word_boundary: bool,
    #[serde(default)]
    pub allow_single: bool,
    #[serde(default)]
    pub requires_participant: bool,
    #[serde(default)]
    pub requires_subject: bool,
}

/// Provenance of a lexeme.
#[derive(Debug, Clone, Deserialize)]
pub struct LexiconSource {
    pub kind: String,
    pub name: String,
}

// ── JSON root structure ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DictionaryFile {
    #[allow(dead_code)]
    _meta: Option<serde_json::Value>,
    #[serde(default)]
    lexemes: Vec<Lexeme>,
    #[serde(default)]
    stop_names: HashMap<String, Vec<String>>,
}

// ── Dictionary struct ───────────────────────────────────────────────────────

/// Runtime dictionary loaded from `config/dictionary.json`.
#[derive(Debug, Clone)]
pub struct Dictionary {
    /// All lexemes indexed by their lemma (lowercased).
    lexemes_by_lemma: HashMap<String, Vec<Lexeme>>,
    /// Lexemes indexed by semantic class.
    lexemes_by_class: HashMap<String, Vec<Lexeme>>,

    /// English stop-words as a sorted `Vec` for binary-search lookup.
    english_stop_names: Vec<String>,
    chinese_stop_names: Vec<String>,

    /// Verb lists kept as `HashSet` for fast membership tests.
    english_strong_verbs: HashSet<String>,
    english_action_verbs: HashSet<String>,
    english_hostile_verbs: HashSet<String>,
    english_friendly_verbs: HashSet<String>,
    chinese_strong_verbs: HashSet<String>,
    chinese_action_verbs: HashSet<String>,
    chinese_hostile_verbs: HashSet<String>,
    chinese_friendly_verbs: HashSet<String>,
}

impl Dictionary {
    /// Load the dictionary from the default path (`config/dictionary.json`).
    pub fn load_default() -> Result<Self, Box<dyn std::error::Error>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        Self::load(&path)
    }

    /// Load the dictionary from a custom JSON path.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let text = std::fs::read_to_string(path)?;
        let file: DictionaryFile = serde_json::from_str(&text)?;

        // Index lexemes.
        let mut lexemes_by_lemma: HashMap<String, Vec<Lexeme>> = HashMap::new();
        let mut lexemes_by_class: HashMap<String, Vec<Lexeme>> = HashMap::new();
        for lex in &file.lexemes {
            let lemma = lex.lemma.to_lowercase();
            lexemes_by_lemma.entry(lemma).or_default().push(lex.clone());
            lexemes_by_class
                .entry(lex.semantic_class.clone())
                .or_default()
                .push(lex.clone());
        }

        // Stop names.
        let en_stop_raw = file.stop_names.get("en").cloned().unwrap_or_default();
        let mut en_stop = en_stop_raw;
        en_stop.sort();

        let zh_stop = file.stop_names.get("zh").cloned().unwrap_or_default();

        // Build flat verb sets from lexemes.
        let mut en_strong = HashSet::new();
        let mut en_action = HashSet::new();
        let mut en_hostile = HashSet::new();
        let mut en_friendly = HashSet::new();
        let mut zh_strong = HashSet::new();
        let mut zh_action = HashSet::new();
        let mut zh_hostile = HashSet::new();
        let mut zh_friendly = HashSet::new();

        for lex in &file.lexemes {
            let all_forms = std::iter::once(&lex.lemma)
                .chain(lex.forms.iter())
                .map(|f| f.to_lowercase())
                .collect::<Vec<_>>();

            let is_attack = lex.semantic_class == "attack";
            let is_rescue = lex.semantic_class == "rescue";
            let is_speech = lex.semantic_class == "speech";
            let is_movement = lex.semantic_class == "movement";
            let is_cognition = lex.semantic_class == "cognition";
            let is_emotion = lex.semantic_class == "emotion";
            let is_transfer = lex.semantic_class == "transfer";
            let is_creation = lex.semantic_class == "creation";
            let is_state = lex.semantic_class == "state";
            let is_intention = lex.semantic_class == "intention";

            let is_strong = is_attack || is_rescue || is_creation;
            let is_action = is_speech
                || is_movement
                || is_emotion
                || is_cognition
                || is_transfer
                || is_state
                || is_intention;

            for form in &all_forms {
                match lex.language.as_str() {
                    "en" => {
                        if is_strong {
                            en_strong.insert(form.clone());
                        }
                        if is_action {
                            en_action.insert(form.clone());
                        }
                        if is_attack {
                            en_hostile.insert(form.clone());
                        }
                        if is_rescue {
                            en_friendly.insert(form.clone());
                        }
                    }
                    "zh" => {
                        if is_strong {
                            zh_strong.insert(form.clone());
                        }
                        if is_action {
                            zh_action.insert(form.clone());
                        }
                        if is_attack {
                            zh_hostile.insert(form.clone());
                        }
                        if is_rescue {
                            zh_friendly.insert(form.clone());
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(Dictionary {
            lexemes_by_lemma,
            lexemes_by_class,
            english_stop_names: en_stop,
            chinese_stop_names: zh_stop,
            english_strong_verbs: en_strong,
            english_action_verbs: en_action,
            english_hostile_verbs: en_hostile,
            english_friendly_verbs: en_friendly,
            chinese_strong_verbs: zh_strong,
            chinese_action_verbs: zh_action,
            chinese_hostile_verbs: zh_hostile,
            chinese_friendly_verbs: zh_friendly,
        })
    }

    // ── Lexeme access ─────────────────────────────────────────────────

    /// Look up all lexemes matching a lemma (case-insensitive).
    pub fn lookup(&self, lemma: &str) -> &[Lexeme] {
        self.lexemes_by_lemma
            .get(&lemma.to_lowercase())
            .map(|v| v.as_slice())
            .unwrap_or_default()
    }

    /// Get all lexemes in a given semantic class.
    pub fn by_class(&self, class: &str) -> &[Lexeme] {
        self.lexemes_by_class
            .get(class)
            .map(|v| v.as_slice())
            .unwrap_or_default()
    }

    // ── Flat accessors ─────────────────────────────────────────────────

    pub fn is_english_stop_name(&self, word: &str) -> bool {
        self.english_stop_names
            .binary_search_by(|s| s.as_str().cmp(word))
            .is_ok()
    }

    pub fn is_chinese_stop_name(&self, word: &str) -> bool {
        self.chinese_stop_names.iter().any(|s| s == word)
    }

    pub fn english_strong_verbs(&self) -> &HashSet<String> {
        &self.english_strong_verbs
    }

    pub fn english_action_verbs(&self) -> &HashSet<String> {
        &self.english_action_verbs
    }

    pub fn english_hostile_verbs(&self) -> &HashSet<String> {
        &self.english_hostile_verbs
    }

    pub fn english_friendly_verbs(&self) -> &HashSet<String> {
        &self.english_friendly_verbs
    }

    pub fn chinese_strong_verbs(&self) -> &HashSet<String> {
        &self.chinese_strong_verbs
    }

    pub fn chinese_action_verbs(&self) -> &HashSet<String> {
        &self.chinese_action_verbs
    }

    pub fn chinese_hostile_verbs(&self) -> &HashSet<String> {
        &self.chinese_hostile_verbs
    }

    pub fn chinese_friendly_verbs(&self) -> &HashSet<String> {
        &self.chinese_friendly_verbs
    }
}

// ── Global singleton ────────────────────────────────────────────────────────

static DICT: LazyLock<RwLock<Dictionary>> = LazyLock::new(|| {
    RwLock::new(
        Dictionary::load_default().expect("config/dictionary.json must be present and valid"),
    )
});

/// Reload the global dictionary from its default path (for hot-reload or testing).
pub fn reload() -> Result<(), Box<dyn std::error::Error>> {
    let dict = Dictionary::load_default()?;
    *DICT.write().unwrap() = dict;
    Ok(())
}

// ── Flat-function API (delegates to global singleton) ───────────────────────

/// Check if `word` is a known English stop-word.
pub fn is_english_stop_name(word: &str) -> bool {
    DICT.read().unwrap().is_english_stop_name(word)
}

/// Check if `word` is a known Chinese stop-word.
pub fn is_chinese_stop_name(word: &str) -> bool {
    DICT.read().unwrap().is_chinese_stop_name(word)
}

/// Access the global dictionary for callers that need the full API.
pub fn global() -> std::sync::RwLockReadGuard<'static, Dictionary> {
    DICT.read().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify the lexeme promotion path follows the plan.
    /// Invariants: Candidate → Experimental → Core; terminal states stay put.
    #[test]
    fn status_promotes_through_lifecycle() {
        assert_eq!(
            LexemeStatus::Candidate.promote(),
            LexemeStatus::Experimental,
            "Candidate promotes to Experimental"
        );
        assert_eq!(
            LexemeStatus::Experimental.promote(),
            LexemeStatus::Core,
            "Experimental promotes to Core"
        );
        assert_eq!(
            LexemeStatus::Core.promote(),
            LexemeStatus::Core,
            "Core is the terminal promotion stage"
        );
        assert_eq!(
            LexemeStatus::Disabled.promote(),
            LexemeStatus::Disabled,
            "Disabled must not be re-promoted"
        );
    }

    /// Objective: Verify the retirement path and active filtering.
    /// Invariants: Core/Experimental → Deprecated → Disabled; only Disabled is inactive.
    #[test]
    fn status_demotes_and_filters_active() {
        assert_eq!(
            LexemeStatus::Core.demote(),
            LexemeStatus::Deprecated,
            "Core demotes to Deprecated"
        );
        assert_eq!(
            LexemeStatus::Deprecated.demote(),
            LexemeStatus::Disabled,
            "Deprecated demotes to Disabled"
        );
        assert_eq!(
            LexemeStatus::Disabled.demote(),
            LexemeStatus::Disabled,
            "Disabled is terminal"
        );

        assert!(LexemeStatus::Core.is_active(), "Core is active");
        assert!(
            LexemeStatus::Deprecated.is_active(),
            "Deprecated stays active until disabled"
        );
        assert!(
            !LexemeStatus::Disabled.is_active(),
            "Disabled must be excluded from matchers"
        );
    }

    /// Objective: Verify JSON round-trips status names.
    /// Invariants: lowercase JSON names deserialize to the right variants.
    #[test]
    fn status_deserializes_from_lowercase() {
        let parsed: LexemeStatus = serde_json::from_str("\"core\"").expect("core parses");
        assert_eq!(parsed, LexemeStatus::Core);
        let parsed: LexemeStatus = serde_json::from_str("\"experimental\"").expect("parses");
        assert_eq!(parsed, LexemeStatus::Experimental);
        let parsed: LexemeStatus = serde_json::from_str("\"disabled\"").expect("parses");
        assert_eq!(parsed, LexemeStatus::Disabled);
    }
}
