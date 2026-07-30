//! Runtime-loaded dictionary for entity-name filtering and language-aware processing.
//!
//! ## Design
//!
//! Word lists (stop-names, verbs) are loaded from `config/dictionary.json` at
//! runtime via a global lazy-static [`Dictionary`] instance. Users can edit
//! the JSON file to add or remove words without recompiling.
//!
//! The module provides a flat-function API (e.g. [`is_english_stop_name`]) that
//! delegates to the global dictionary, so existing callers need no changes.

use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;
use std::sync::RwLock;

use serde::Deserialize;

// ── Data structure ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct DictionaryData {
    english_stop_names: Vec<String>,
    chinese_stop_names: Vec<String>,
    english_strong_verbs: Vec<String>,
    english_action_verbs: Vec<String>,
    english_hostile_verbs: Vec<String>,
    english_friendly_verbs: Vec<String>,
    chinese_strong_verbs: Vec<String>,
    chinese_action_verbs: Vec<String>,
    chinese_hostile_verbs: Vec<String>,
    chinese_friendly_verbs: Vec<String>,
}

/// Runtime dictionary loaded from `config/dictionary.json`.
#[derive(Debug, Clone)]
pub struct Dictionary {
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
        let data: DictionaryData = serde_json::from_str(&text)?;
        let mut stop = data.english_stop_names;
        stop.sort();
        Ok(Dictionary {
            english_stop_names: stop,
            chinese_stop_names: data.chinese_stop_names,
            english_strong_verbs: data.english_strong_verbs.into_iter().collect(),
            english_action_verbs: data.english_action_verbs.into_iter().collect(),
            english_hostile_verbs: data.english_hostile_verbs.into_iter().collect(),
            english_friendly_verbs: data.english_friendly_verbs.into_iter().collect(),
            chinese_strong_verbs: data.chinese_strong_verbs.into_iter().collect(),
            chinese_action_verbs: data.chinese_action_verbs.into_iter().collect(),
            chinese_hostile_verbs: data.chinese_hostile_verbs.into_iter().collect(),
            chinese_friendly_verbs: data.chinese_friendly_verbs.into_iter().collect(),
        })
    }

    // ── Accessors ───────────────────────────────────────────────────────

    pub fn is_english_stop_name(&self, word: &str) -> bool {
        self.english_stop_names.binary_search_by(|s| s.as_str().cmp(word)).is_ok()
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

    /// Return all English strong verbs as a sorted slice for iterating.
    pub fn english_strong_verbs_sorted(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.english_strong_verbs.iter().map(String::as_str).collect();
        v.sort();
        v
    }

    /// Return all English action verbs as a sorted slice.
    pub fn english_action_verbs_sorted(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.english_action_verbs.iter().map(String::as_str).collect();
        v.sort();
        v
    }

    /// Return all Chinese strong verbs as a sorted slice.
    pub fn chinese_strong_verbs_sorted(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.chinese_strong_verbs.iter().map(String::as_str).collect();
        v.sort();
        v
    }

    /// Return all Chinese action verbs as a sorted slice.
    pub fn chinese_action_verbs_sorted(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.chinese_action_verbs.iter().map(String::as_str).collect();
        v.sort();
        v
    }
}

// ── Global singleton ────────────────────────────────────────────────────────

static DICT: LazyLock<RwLock<Dictionary>> = LazyLock::new(|| {
    RwLock::new(
        Dictionary::load_default()
            .expect("config/dictionary.json must be present and valid"),
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

/// Access the global dictionary for callers that need the full lists.
pub fn global() -> std::sync::RwLockReadGuard<'static, Dictionary> {
    DICT.read().unwrap()
}
