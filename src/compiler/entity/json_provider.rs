//! JSON entity provider — loads entity definitions from `config/entity_profiles/*.json`.
//!
//! Each JSON file defines a named profile with a list of entities (canonical
//! name, aliases, single-char shortname, object type). Multiple profiles can
//! be registered in the [`EntityRegistry`](super::EntityRegistry) for a single
//! compilation.
//!
//! ## Format
//!
//! ```json
//! {
//!   "profile_name": "西游记",
//!   "doc_type_hint": "novel",
//!   "entities": [
//!     {"canonical_name":"孙悟空","aliases":["悟空","行者"],"object_type":"person"}
//!   ]
//! }
//! ```

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use super::provider::{EntityEntry, EntityProvider};

/// A single entity entry in a JSON profile.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsonEntity {
    canonical_name: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    single_char: Option<String>,
    #[serde(default = "default_object_type")]
    object_type: String,
}

fn default_object_type() -> String {
    "person".into()
}

/// A JSON entity profile file.
#[derive(Debug, Clone, Deserialize)]
pub struct EntityProfile {
    pub profile_name: String,
    #[serde(default)]
    pub _doc_type_hint: String,
    #[serde(default = "default_empty_verbs")]
    pub verbs: Vec<Vec<String>>,
    #[serde(default)]
    pub hostile_verbs: Vec<String>,
    #[serde(default)]
    pub friendly_verbs: Vec<String>,
    #[serde(default)]
    pub faction_switch_triggers: Option<HashMap<String, Vec<String>>>,
    #[serde(default)]
    pub personality_patterns: Vec<String>,
    #[serde(default)]
    pub profile_patterns: Vec<serde_json::Value>,
    pub entities: Vec<JsonEntity>,
}

fn default_empty_verbs() -> Vec<Vec<String>> {
    Vec::new()
}

/// Provider that loads entity definitions from a JSON file.
///
/// # Example
///
/// ```ignore
/// let provider = JsonEntityProvider::from_file("config/entity_profiles/xiyou.json")?;
/// registry.register(Arc::new(provider));
/// ```
pub struct JsonEntityProvider {
    name: String,
    entries: Vec<EntityEntry>,
    /// Strong/dialog/action verb groups loaded from the profile.
    /// Index 0 = strong_verbs, 1 = dialog_verbs, 2 = action_verbs.
    verb_groups: Vec<Vec<String>>,
    /// Verbs that indicate hostile relationship changes (杀, 斩, 攻...).
    hostile_verbs: Vec<String>,
    /// Verbs that indicate friendly relationship changes (救, 拜, 封...).
    friendly_verbs: Vec<String>,
    /// Faction switch trigger patterns by type (surrender, betrayal, ...).
    faction_switch_triggers: HashMap<String, Vec<String>>,
    /// Personality pattern strings (性, 为人, 平生...) for character arc detection.
    personality_patterns: Vec<String>,
    /// Profile extraction patterns (used by profile.rs when non-empty).
    profile_patterns: Vec<serde_json::Value>,
}

impl JsonEntityProvider {
    /// Load a profile from a JSON file path.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the file cannot be read, or a parse error if the
    /// JSON is malformed.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path.as_ref())?;
        Self::from_json(&content)
    }

    /// Load a profile from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let profile: EntityProfile = serde_json::from_str(json)?;
        let entries = profile
            .entities
            .into_iter()
            .map(|e| {
                let mut properties = HashMap::new();
                properties.insert("profile".to_string(), profile.profile_name.clone());
                EntityEntry {
                    canonical_name: e.canonical_name,
                    aliases: e.aliases,
                    single_char: e.single_char,
                    object_type: e.object_type,
                    properties,
                }
            })
            .collect();
        Ok(JsonEntityProvider {
            name: profile.profile_name,
            entries,
            verb_groups: profile.verbs,
            hostile_verbs: profile.hostile_verbs,
            friendly_verbs: profile.friendly_verbs,
            faction_switch_triggers: profile.faction_switch_triggers.unwrap_or_default(),
            personality_patterns: profile.personality_patterns,
            profile_patterns: profile.profile_patterns,
        })
    }

    /// Return the verb groups: index 0 = strong_verbs, 1 = dialog_verbs, 2 = action_verbs.
    pub fn verbs(&self) -> &[Vec<String>] {
        &self.verb_groups
    }

    /// Return the hostile verbs (杀, 斩, 攻...) for timeline building.
    pub fn hostile_verbs(&self) -> &[String] {
        &self.hostile_verbs
    }

    /// Return the friendly verbs (救, 拜, 封...) for timeline building.
    pub fn friendly_verbs(&self) -> &[String] {
        &self.friendly_verbs
    }

    /// Return the faction switch triggers: {"surrender": [...], "betrayal": [...]}
    pub fn faction_switch_triggers(&self) -> &HashMap<String, Vec<String>> {
        &self.faction_switch_triggers
    }

    /// Return the personality patterns (性, 为人, ...) for character arc detection.
    pub fn personality_patterns(&self) -> &[String] {
        &self.personality_patterns
    }

    /// Return profile extraction patterns as deserialized [`ProfilePattern`]s.
    #[cfg(feature = "remote-embed")]
    pub fn profile_patterns(&self) -> Vec<crate::compiler::profile::ProfilePattern> {
        self.profile_patterns
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect()
    }

    /// Build an observation config from the profile's verb groups.
    pub fn observation_config(&self) -> Vec<Vec<String>> {
        self.verb_groups.clone()
    }

    /// Load all JSON files from a directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory doesn't exist or if any file fails
    /// to parse.
    pub fn from_directory(dir: impl AsRef<Path>) -> Result<Vec<Self>, Box<dyn std::error::Error>> {
        let mut providers = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                providers.push(Self::from_file(&path)?);
            }
        }
        Ok(providers)
    }
}

impl EntityProvider for JsonEntityProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn entries(&self) -> Vec<EntityEntry> {
        self.entries.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify that a valid JSON profile produces the expected entries.
    /// Invariants: The provider returns entries matching the JSON content.
    #[test]
    fn load_xiyou_profile() {
        let json = r#"{
            "profile_name": "西游记",
            "entities": [
                {"canonical_name":"孙悟空","aliases":["悟空","行者"],"object_type":"person"},
                {"canonical_name":"辟寒大王","aliases":["辟寒儿","辟寒"],"object_type":"person"}
            ]
        }"#;
        let provider = JsonEntityProvider::from_json(json).expect("parse xiyou profile");
        assert_eq!(provider.name(), "西游记");
        let entries = provider.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].canonical_name, "孙悟空");
        assert_eq!(entries[1].canonical_name, "辟寒大王");
    }

    /// Objective: Verify that loading from a non-existent file returns an error.
    /// Invariants: Result is Err, not a panic.
    #[test]
    fn missing_file_returns_error() {
        let result = JsonEntityProvider::from_file("/nonexistent/path.json");
        assert!(result.is_err(), "missing file should error");
    }

    /// Objective: Verify that from_directory loads all JSON files.
    /// Invariants: At least one provider is loaded from the config directory.
    #[test]
    fn load_directory() {
        let providers = JsonEntityProvider::from_directory("config/entity_profiles")
            .expect("load config/entity_profiles");
        assert!(!providers.is_empty(), "should load at least one profile");
        let names: Vec<&str> = providers.iter().map(|p| p.name()).collect();
        assert!(names.contains(&"西游记"), "should find 西游记 profile");
        assert!(names.contains(&"三国演义"), "should find 三国演义 profile");
    }
}
