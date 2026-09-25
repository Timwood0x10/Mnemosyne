//! Person-name validation tables, loaded from JSON.
//!
//! The surname / function-word / noun-tail lists used by
//! [`is_valid_person_name`] live in `config/name_validation.json` so users can
//! extend them without touching code. Loading mirrors
//! `ingest/relation.rs`: env-var path override → default JSON → built-in
//! fallback, cached once per process via [`std::sync::LazyLock`].
//!
//! ## Customization
//!
//! - Edit `config/name_validation.json` directly, or
//! - Set `NAME_VALIDATION_PATH` to point at your own JSON file.
//!
//! The fallback tables below are only used when the JSON is missing or
//! unparseable — they keep the validator functional, never the primary
//! configuration.

use std::sync::LazyLock;

use serde::Deserialize;

/// JSON shape of `config/name_validation.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct NameValidationConfig {
    /// Valid surname strings (incl. compound like `公孙`).
    pub surnames: Vec<String>,
    /// Function words that can never appear inside a person name.
    pub function_words: Vec<String>,
    /// Noun/measure-word tails that indicate a phrase, not a name.
    pub noun_tails: Vec<String>,
}

/// Load the config: resource root `config/name_validation.json` → fallback.
fn load_config() -> NameValidationConfig {
    let path = crate::config::resolve_resource_path("config/name_validation.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(fallback_config)
}

/// Cached, process-wide config (P3: never re-read the file per line).
static CONFIG: LazyLock<NameValidationConfig> = LazyLock::new(load_config);

mod fallback;

use fallback::fallback_config;

/// Shared C1/C3/C4 gates (shape, no function words, no noun tail).
/// Returns `None` when a gate fails; `Some(())` when the soft checks pass.
fn soft_gates_pass(name: &str) -> Option<()> {
    let chars: Vec<char> = name.chars().collect();
    // C1: shape.
    if !(2..=4).contains(&chars.len()) {
        return None;
    }
    if !chars.iter().all(|c| ('\u{4e00}'..='\u{9fff}').contains(c)) {
        return None;
    }
    let cfg = &*CONFIG;
    // C3: function words anywhere.
    if chars
        .iter()
        .any(|c| cfg.function_words.iter().any(|w| w.starts_with(*c)))
    {
        return None;
    }
    // C4: noun tail.
    if cfg.noun_tails.iter().any(|t| name.ends_with(t.as_str())) {
        return None;
    }
    Some(())
}

/// Validate that a heuristically discovered entity name looks like a real
/// person name. Four gates, conservative by design:
///
/// - C1 shape: 2–4 CJK chars.
/// - C2 surname-led: first 1–2 chars are a known surname (incl. compound).
/// - C3 given-name purity: no function words anywhere in the name.
/// - C4 noun-tail rejection: the last char is not a common noun/measure tail.
///
/// Tables come from `config/name_validation.json` (user-extensible); the
/// built-in fallback keeps the validator functional when the file is absent.
#[must_use]
pub fn is_valid_person_name(name: &str) -> bool {
    if soft_gates_pass(name).is_none() {
        return false;
    }
    let cfg = &*CONFIG;
    let chars: Vec<char> = name.chars().collect();
    // C2: surname-led (check 2-char compound first, then single char).
    let head2: String = chars[..chars.len().min(2)].iter().collect();
    if cfg.surnames.contains(&head2) {
        return true;
    }
    let first = chars[0].to_string();
    cfg.surnames.contains(&first)
}

/// Soft person-name check for corpus speaker discovery: same C1/C3/C4 gates
/// as [`is_valid_person_name`] but WITHOUT requiring a known surname.
///
/// Literary given names used as dialogue speakers ("流苏说道") often have no
/// standard surname; requiring C2 rejected the entire cast of 倾城之恋 while
/// still needed to stop garbage like "感觉". Frequency + NON_NAMES still apply
/// at the call site.
#[must_use]
pub fn is_plausible_person_name(name: &str) -> bool {
    soft_gates_pass(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify the JSON config file parses and carries the expected
    /// tables.
    /// Invariants: surnames non-empty and contain a compound (公孙), function
    /// words contain 的, noun tails contain 角.
    #[test]
    fn json_config_loads() {
        let path = std::env::var("NAME_VALIDATION_PATH")
            .unwrap_or_else(|_| "config/name_validation.json".to_string());
        let s = std::fs::read_to_string(&path).expect("name_validation.json must exist");
        let cfg: NameValidationConfig = serde_json::from_str(&s).expect("valid JSON");
        assert!(!cfg.surnames.is_empty(), "surnames table non-empty");
        assert!(
            cfg.surnames.iter().any(|s| s == "公孙"),
            "compound surname 公孙 present"
        );
        assert!(
            cfg.function_words.iter().any(|w| w == "的"),
            "function word 的 present"
        );
        assert!(
            cfg.noun_tails.iter().any(|t| t == "角"),
            "noun tail 角 present"
        );
    }

    /// Objective: Verify the validator behaves identically to the hardcoded
    /// version (regression lock).
    /// Invariants: 嬴渠梁/公孙鞅 pass; 涓的秘密/牛角/文明载体 rejected.
    #[test]
    fn validator_regression() {
        assert!(is_valid_person_name("嬴渠梁"), "嬴渠梁 valid");
        assert!(is_valid_person_name("公孙鞅"), "compound surname valid");
        assert!(is_valid_person_name("李金诚"), "mid-name noun char valid");
        assert!(
            !is_valid_person_name("涓的秘密"),
            "function-word tail rejected"
        );
        assert!(!is_valid_person_name("牛角"), "noun tail rejected");
        assert!(!is_valid_person_name("文明载体"), "phrase tail 体 rejected");
        assert!(!is_valid_person_name("一"), "single char rejected");
    }

    /// Objective: Verify the soft corpus gate accepts literary given names
    /// without a surname while still rejecting function-word/noun garbage.
    /// Invariants: 流苏/感觉-path: 流苏 soft-passes; 的-tail and 角-tail fail.
    #[test]
    fn soft_gate_accepts_literary_names() {
        assert!(
            is_plausible_person_name("流苏"),
            "literary given name 流苏 accepted by soft gate"
        );
        assert!(
            is_plausible_person_name("玄德"),
            "courtesy name 玄德 accepted by soft gate"
        );
        assert!(
            !is_plausible_person_name("涓的秘密"),
            "function-word tail still rejected"
        );
        assert!(
            !is_plausible_person_name("牛角"),
            "noun tail still rejected"
        );
    }
}
