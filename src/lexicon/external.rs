//! External lexicon adapters (P5 — open reference dictionaries).
//!
//! ## Design
//!
//! External reference dictionaries (e.g. Princeton WordNet, Open English
//! WordNet, user-supplied word lists) provide **auxiliary** information only:
//! lemma, part of speech, and synonyms. They NEVER decide cognitive facts —
//! only the core/domain/user lexemes with explicit `CognitiveEffect` entries
//! can do that (see [`crate::lexicon::Lexeme`]).
//!
//! This module defines:
//!
//! - [`ExternalEntry`] — a row from an external dictionary (no effects).
//! - [`ExternalLexiconProvider`] — the adapter trait.
//! - [`ExternalFileProvider`] — a JSON-file-backed implementation.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::lexicon::LexiconError;

/// A single row from an external dictionary.
///
/// Deliberately **has no `CognitiveEffect`** — external data must never
/// silently change FactType/RelationType output.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ExternalEntry {
    /// Surface word form.
    pub word: String,
    /// Optional lemma (canonical form).
    #[serde(default)]
    pub lemma: Option<String>,
    /// Optional part of speech (e.g. "verb", "noun").
    #[serde(default)]
    pub part_of_speech: Option<String>,
    /// Synonyms for candidate generation.
    #[serde(default)]
    pub synonyms: Vec<String>,
}

/// JSON file structure: `{ "entries": [...] }` or a bare array.
#[derive(Debug, Deserialize)]
struct ExternalFile {
    #[serde(default)]
    entries: Vec<ExternalEntry>,
}

/// Adapter trait for external lexicon providers.
pub trait ExternalLexiconProvider: Send + Sync {
    /// Provider name (e.g. "wordnet", "user_dictionary").
    fn name(&self) -> &str;

    /// Look up entries for a surface word (case-insensitive).
    fn lookup(&self, word: &str) -> Vec<&ExternalEntry>;

    /// List candidate surface words matching a prefix (for suggestion).
    fn candidates(&self, prefix: &str, limit: usize) -> Vec<String>;
}

/// JSON-file-backed external lexicon.
///
/// Loads entries once at construction; lookups are O(1) via a HashMap.
/// The file is expected to be a JSON object `{"entries": [...]}` or a bare
/// array of entries.
pub struct ExternalFileProvider {
    name: String,
    by_word: HashMap<String, Vec<ExternalEntry>>,
    all_words: Vec<String>,
}

impl ExternalFileProvider {
    /// Load a provider from a JSON file.
    ///
    /// # Errors
    ///
    /// Returns [`LexiconError::FileLoad`] if the file cannot be read, or
    /// [`LexiconError::Parse`] if its JSON does not match the expected shape.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, LexiconError> {
        let text = std::fs::read_to_string(path.as_ref()).map_err(|e| LexiconError::FileLoad {
            path: path.as_ref().display().to_string(),
            cause: e.to_string(),
        })?;
        let name = path
            .as_ref()
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "external".to_string());
        Self::from_json(&name, &text)
    }

    /// Build a provider from a JSON string with an explicit name.
    ///
    /// # Errors
    ///
    /// Returns [`LexiconError::Parse`] if the JSON does not match the
    /// expected shape.
    pub fn from_json(name: &str, json: &str) -> Result<Self, LexiconError> {
        let entries: Vec<ExternalEntry> = match serde_json::from_str::<ExternalFile>(json) {
            Ok(file) => file.entries,
            Err(_) => serde_json::from_str::<Vec<ExternalEntry>>(json).map_err(|e| {
                LexiconError::Parse {
                    detail: format!("external lexicon `{name}`: {e}"),
                }
            })?,
        };

        let mut by_word: HashMap<String, Vec<ExternalEntry>> = HashMap::new();
        let mut all_words: Vec<String> = Vec::new();
        for entry in entries {
            let key = entry.word.to_lowercase();
            by_word.entry(key.clone()).or_default().push(entry);
            all_words.push(key);
        }
        all_words.sort();
        all_words.dedup();

        Ok(ExternalFileProvider {
            name: name.to_string(),
            by_word,
            all_words,
        })
    }
}

impl ExternalLexiconProvider for ExternalFileProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn lookup(&self, word: &str) -> Vec<&ExternalEntry> {
        self.by_word
            .get(&word.to_lowercase())
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }

    fn candidates(&self, prefix: &str, limit: usize) -> Vec<String> {
        let prefix = prefix.to_lowercase();
        self.all_words
            .iter()
            .filter(|w| w.starts_with(&prefix))
            .take(limit)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify a JSON external lexicon loads and answers lookups.
    /// Invariants: Lookup is case-insensitive; unknown words return empty.
    #[test]
    fn file_provider_loads_and_looks_up() {
        let json = r#"{
            "entries": [
                {"word": "kill", "lemma": "kill", "part_of_speech": "verb", "synonyms": ["murder", "slay"]},
                {"word": "beautiful", "lemma": "beautiful", "part_of_speech": "adjective"}
            ]
        }"#;
        let provider = ExternalFileProvider::from_json("test", json)
            .expect("external lexicon must parse");

        assert_eq!(provider.name(), "test");
        let kill = provider.lookup("KILL"); // case-insensitive
        assert_eq!(kill.len(), 1, "case-insensitive lookup for KILL");
        assert_eq!(kill[0].word, "kill");
        assert_eq!(
            kill[0].synonyms,
            vec!["murder".to_string(), "slay".to_string()],
            "synonyms must be preserved"
        );
        assert!(
            provider.lookup("nonexistent").is_empty(),
            "unknown word must return empty"
        );
    }

    /// Objective: Verify bare-array JSON is accepted.
    /// Invariants: A top-level array of entries parses the same as an object.
    #[test]
    fn bare_array_json_is_accepted() {
        let json = r#"[
            {"word": "run", "lemma": "run", "part_of_speech": "verb"}
        ]"#;
        let provider = ExternalFileProvider::from_json("arr", json)
            .expect("bare array must parse");
        assert_eq!(provider.lookup("run").len(), 1);
    }

    /// Objective: Verify prefix candidate suggestions.
    /// Invariants: Candidates are sorted and deduplicated; limit is honored.
    #[test]
    fn candidates_are_sorted_and_limited() {
        let json = r#"{
            "entries": [
                {"word": "apple"},
                {"word": "application"},
                {"word": "banana"}
            ]
        }"#;
        let provider = ExternalFileProvider::from_json("cand", json)
            .expect("must parse");

        let hits = provider.candidates("app", 10);
        assert_eq!(
            hits,
            vec!["apple".to_string(), "application".to_string()],
            "prefix candidates must be sorted and deduplicated"
        );

        let limited = provider.candidates("a", 1);
        assert_eq!(limited.len(), 1, "limit must be honored");
    }

    /// Objective: Verify malformed JSON yields a Parse error, not a panic.
    /// Invariants: Invalid input returns Err with a descriptive message.
    #[test]
    fn malformed_json_returns_error() {
        let result = ExternalFileProvider::from_json("bad", "not json at all");
        assert!(result.is_err(), "malformed JSON must error");
    }
}
