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
use std::sync::{LazyLock, Mutex, RwLock};

use serde::{Deserialize, Serialize};

// ── Lexeme types ────────────────────────────────────────────────────────────

/// Lifecycle stage of a lexeme (ELITE_LEXICON_PLAN §10).
///
/// Promotion path: `Candidate → Experimental → Core`.
/// Retirement path: `Core/Experimental → Deprecated → Disabled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
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
    ///
    /// Only `Core` (shipped in the default core) enters `Deprecated` — the
    /// grace state for lexemes scheduled for removal. `Experimental` lexemes
    /// never shipped, so they retire directly to `Disabled`; this guarantees
    /// that `Deprecated` always implies a `Core` origin, which keeps
    /// [`Self::rollback`] exact (no silent Experimental→Core promotion).
    pub fn demote(self) -> LexemeStatus {
        match self {
            LexemeStatus::Core => LexemeStatus::Deprecated,
            LexemeStatus::Experimental | LexemeStatus::Deprecated => LexemeStatus::Disabled,
            other => other,
        }
    }

    /// Roll back one step along the retirement path (P6 rollback mechanism).
    ///
    /// A `Deprecated` lexeme is reinstated to `Core` (its only possible
    /// pre-deprecation state, since only Core enters Deprecated); a `Disabled`
    /// lexeme is first restored to `Deprecated` so it stays visible in
    /// manifests for at least one release before being fully re-activated.
    pub fn rollback(self) -> LexemeStatus {
        match self {
            LexemeStatus::Deprecated => LexemeStatus::Core,
            LexemeStatus::Disabled => LexemeStatus::Deprecated,
            other => other,
        }
    }
}

/// A single entry in the Elite Lexicon.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CognitiveEffect {
    #[serde(rename = "type")]
    pub effect_type: String,
    pub value: String,
}

/// Match-time constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LexiconSource {
    pub kind: String,
    pub name: String,
}

// ── JSON root structure ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DictionaryFile {
    // The `_meta` documentation key needs no field: serde ignores unknown keys,
    // and declaring it dead weight only invited a `#[allow(dead_code)]`.
    #[serde(default)]
    lexemes: Vec<Lexeme>,
    #[serde(default)]
    stop_names: HashMap<String, Vec<String>>,
}

// ── Dictionary struct ───────────────────────────────────────────────────────

/// Runtime dictionary loaded from `config/dictionary.json`.
#[derive(Debug, Clone, Default)]
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
    ///
    /// The path is resolved at runtime from the resource root — the
    /// `MNEMOSYNE_HOME` env override, else the install root — NOT a
    /// compile-time `env!("CARGO_MANIFEST_DIR")` baked into the binary (which
    /// made every release fail except on the CI builder).
    pub fn load_default() -> Result<Self, Box<dyn std::error::Error>> {
        let path = crate::config::resolve_resource_path("config/dictionary.json");
        Self::load(&path)
    }

    /// Load the dictionary from a custom JSON path.
    ///
    /// Lexemes whose `status` is [`LexemeStatus::Disabled`] are dropped here,
    /// so they contribute neither to the lemma/class indexes nor to the flat
    /// verb sets. `Disabled` documents itself as "excluded from matchers
    /// entirely" (ELITE_LEXICON_PLAN §10) and is the only user-facing way to
    /// turn a shipped word off, so `is_active` must gate every consumer.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let text = std::fs::read_to_string(path)?;
        let file: DictionaryFile = serde_json::from_str(&text)?;

        // Index lexemes (skipping disabled ones entirely).
        let mut lexemes_by_lemma: HashMap<String, Vec<Lexeme>> = HashMap::new();
        let mut lexemes_by_class: HashMap<String, Vec<Lexeme>> = HashMap::new();
        for lex in file.lexemes.iter().filter(|lex| lex.status.is_active()) {
            let lemma = lex.lemma.to_lowercase();
            lexemes_by_lemma.entry(lemma).or_default().push(lex.clone());
            lexemes_by_class
                .entry(lex.semantic_class.clone())
                .or_default()
                .push(lex.clone());
        }

        // Stop names. Normalize English entries to lowercase so the
        // case-sensitive `binary_search` in `is_english_stop_name` matches
        // probes like "You"/"My" (callers only pass capitalized words).
        let en_stop_raw = file.stop_names.get("en").cloned().unwrap_or_default();
        let mut en_stop: Vec<String> = en_stop_raw.into_iter().map(|s| s.to_lowercase()).collect();
        en_stop.sort();
        en_stop.dedup();

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

        for lex in file.lexemes.iter().filter(|lex| lex.status.is_active()) {
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
            .binary_search_by(|s| s.as_str().cmp(&word.to_lowercase()))
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
    // Two different situations, two different severities:
    //
    // - ABSENT: a legitimate minimal deployment with no vocabulary at all. The
    //   engine still runs, so this is a warning, NOT a startup error.
    // - PRESENT BUT UNPARSABLE: the operator edited the file and broke it. We
    //   still degrade to an EMPTY dictionary (stop-word/verb lookups just miss)
    //   so a request never panics, but the cause is logged at ERROR level AND
    //   stashed in `LOAD_ERROR` so startup code turns it into a hard failure
    //   via [`try_init`] — a broken file must not "think it loaded" (H18).
    let path = crate::config::resolve_resource_path("config/dictionary.json");
    if !path.exists() {
        tracing::warn!(
            path = %path.display(),
            "config/dictionary.json is absent; using an empty dictionary"
        );
        return RwLock::new(Dictionary::default());
    }
    let dict = match Dictionary::load(&path) {
        Ok(d) => d,
        Err(e) => {
            let msg = format!("config/dictionary.json exists but failed to load: {e}");
            tracing::error!(
                error = %e,
                path = %path.display(),
                "config/dictionary.json exists but failed to load; using an empty dictionary"
            );
            record_load_error(msg);
            Dictionary::default()
        }
    };
    RwLock::new(dict)
});

/// The load failure recorded by the [`DICT`] initializer, if any.
static LOAD_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// Stash the singleton's load failure so [`try_init`] can surface it.
fn record_load_error(message: String) {
    // The slot is written once, during `DICT` initialization, and only read
    // afterwards; no user code runs while the guard is held, so it cannot be
    // poisoned. A poisoned lock is treated as "no recorded error".
    if let Ok(mut slot) = LOAD_ERROR.lock() {
        *slot = Some(message);
    }
}

/// Force the global dictionary to load and report whether it succeeded.
///
/// Startup code calls this so a broken dictionary becomes a visible startup
/// failure instead of a silent one. Returns `Err(reason)` only when
/// `config/dictionary.json` EXISTS but could not be parsed; an absent file is a
/// legitimate minimal deployment and returns `Ok(())`.
pub fn try_init() -> Result<(), String> {
    // Touch the LazyLock so the load happens now, not on the first request.
    let loaded = DICT.read().is_ok();
    if !loaded {
        return Err("global dictionary read lock is poisoned".to_string());
    }
    match LOAD_ERROR.lock() {
        Ok(slot) => match slot.as_ref() {
            Some(reason) => Err(reason.clone()),
            None => Ok(()),
        },
        // A poisoned slot means another thread panicked mid-write; the
        // dictionary itself loaded (see above), so do not fail the process.
        Err(_) => Ok(()),
    }
}

/// Reload the global dictionary from its default path (for hot-reload or testing).
pub fn reload() -> Result<(), Box<dyn std::error::Error>> {
    let dict = Dictionary::load_default()?;
    // RwLock poisoning requires a panic while the write guard is held; the
    // assignment below cannot panic, so this expect never fires.
    *DICT
        .write()
        .expect("global dictionary write lock is not poisoned") = dict;
    Ok(())
}

// ── Flat-function API (delegates to global singleton) ───────────────────────

/// Check if `word` is a known English stop-word.
pub fn is_english_stop_name(word: &str) -> bool {
    // Read guards are released before any user code runs again, so a poisoned
    // lock is impossible in practice; expect documents the invariant.
    DICT.read()
        .expect("global dictionary read lock is not poisoned")
        .is_english_stop_name(word)
}

/// Check if `word` is a known Chinese stop-word.
pub fn is_chinese_stop_name(word: &str) -> bool {
    // See `is_english_stop_name` — read-guard unwrap cannot panic.
    DICT.read()
        .expect("global dictionary read lock is not poisoned")
        .is_chinese_stop_name(word)
}

/// Access the global dictionary for callers that need the full API.
pub fn global() -> std::sync::RwLockReadGuard<'static, Dictionary> {
    // See `is_english_stop_name` — read-guard unwrap cannot panic.
    DICT.read()
        .expect("global dictionary read lock is not poisoned")
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

    /// Objective: Verify the rollback path reverses retirement (P6).
    /// Invariants: Deprecated → Core (reinstated); Disabled → Deprecated
    /// (one release grace); active statuses are unaffected.
    #[test]
    fn status_rolls_back_retirement() {
        assert_eq!(
            LexemeStatus::Deprecated.rollback(),
            LexemeStatus::Core,
            "Deprecated rolls back to Core (reinstated)"
        );
        assert_eq!(
            LexemeStatus::Disabled.rollback(),
            LexemeStatus::Deprecated,
            "Disabled rolls back to Deprecated first (grace period)"
        );
        assert_eq!(
            LexemeStatus::Core.rollback(),
            LexemeStatus::Core,
            "Core is unaffected by rollback"
        );
        assert_eq!(
            LexemeStatus::Candidate.rollback(),
            LexemeStatus::Candidate,
            "Candidate is unaffected by rollback"
        );

        // demote → rollback round-trips Core back to Core.
        let round_trip = LexemeStatus::Core.demote().rollback();
        assert_eq!(
            round_trip,
            LexemeStatus::Core,
            "demote+rollback restores Core"
        );

        // An Experimental lexeme (never shipped in core) retires directly to
        // Disabled — it must NOT enter Deprecated, so rollback can never
        // silently promote it to Core (regression for the P3 finding).
        assert_eq!(
            LexemeStatus::Experimental.demote(),
            LexemeStatus::Disabled,
            "Experimental retires directly to Disabled (no Core-only grace)"
        );
        let experimental_round_trip = LexemeStatus::Experimental.demote().rollback();
        assert_eq!(
            experimental_round_trip,
            LexemeStatus::Deprecated,
            "Experimental demote+rollback lands on Deprecated, never Core"
        );
    }

    /// A minimal attack lexeme JSON object with the given id/lemma/status.
    fn lexeme_json(id: &str, lemma: &str, status: &str) -> String {
        format!(
            r#"{{"id":"{id}","language":"en","lemma":"{lemma}","pos":"verb","semantic_class":"attack","effects":[],"polarity":"neutral","priority":500,"constraints":{{"word_boundary":true,"allow_single":false,"requires_participant":false,"requires_subject":false}},"source":{{"kind":"builtin","name":"test"}},"status":"{status}"}}"#
        )
    }

    /// Objective: Verify a `Disabled` lexeme contributes to no index or verb set,
    /// so marking a shipped word `"status": "disabled"` really turns it off.
    /// `is_active` had no production caller, so the flag documented as
    /// "excluded from matchers entirely" did nothing (audit H17).
    /// Invariants: the active lemma is visible; the disabled one is absent from
    /// both `lookup` and the hostile-verb set.
    #[test]
    fn disabled_lexemes_are_excluded_from_the_dictionary() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("dictionary.json");
        let body = format!(
            r#"{{"lexemes":[{},{}]}}"#,
            lexeme_json("en.test.active", "zzzactiveverb", "core"),
            lexeme_json("en.test.disabled", "zzzdisabledverb", "disabled")
        );
        std::fs::write(&path, body).expect("write fixture");

        let dict = Dictionary::load(&path).expect("fixture must load");

        assert!(
            !dict.lookup("zzzactiveverb").is_empty(),
            "an active lexeme must be indexed"
        );
        assert!(
            dict.lookup("zzzdisabledverb").is_empty(),
            "a disabled lexeme must not be indexed"
        );
        assert!(
            dict.english_hostile_verbs().contains("zzzactiveverb"),
            "an active attack lexeme must reach the hostile verb set"
        );
        assert!(
            !dict.english_hostile_verbs().contains("zzzdisabledverb"),
            "a disabled attack lexeme must not reach the hostile verb set"
        );
    }

    /// Objective: Verify the failure path `try_init` surfaces — a malformed
    /// dictionary file is reported as an error rather than silently becoming an
    /// empty table (audit H18).
    /// Invariants: `Dictionary::load` returns `Err` for invalid JSON.
    #[test]
    fn load_reports_malformed_json() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("broken.json");
        std::fs::write(&path, r#"{"lexemes": [ this is not json"#).expect("write broken fixture");

        let result = Dictionary::load(&path);
        assert!(
            result.is_err(),
            "malformed JSON must surface as an Err, got {result:?}"
        );
    }

    /// Objective: Verify the global singleton reports a clean load when the
    /// shipped config is present, so `try_init` is `Ok` in the normal case.
    /// Invariants: `try_init()` returns `Ok(())`.
    #[test]
    fn try_init_succeeds_with_the_shipped_config() {
        assert!(
            try_init().is_ok(),
            "the shipped config/dictionary.json must load cleanly"
        );
    }
}
