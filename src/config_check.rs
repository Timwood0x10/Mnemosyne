//! `config-check`: report how the runtime resource files were loaded.
//!
//! The vocabulary lives in `config/`, so customising the engine means editing
//! data rather than code. This is the feedback loop for that: it prints which
//! files were read, what each one was worth, the effective word count per
//! action, and every entry that was dropped or ignored — then fails the process
//! when the table cannot be trusted.
//!
//! Read-only: it never rewrites a config file, and it never starts the server.

use std::path::{Path, PathBuf};

use crate::config::resolve_resource_path;
use crate::conversation_compiler::{MarkerReport, inspect_markers};
use crate::observation_compiler::OBSERVATION_ACTIONS;

/// Resource files the runtime reads, and what each one is for.
///
/// Listed here so "which files are supposed to exist?" is answerable without
/// reading the loaders. Absence is not an error for any of them — the note on
/// the marker rows is the one case where absence changes what gets remembered.
const RESOURCE_FILES: &[(&str, &str)] = &[
    ("config/markers_zh.json", "observation markers (Chinese)"),
    ("config/markers_en.json", "observation markers (English)"),
    (
        "config/*.user.json",
        "your own marker tables (merged last; `_remove` deletes)",
    ),
    (
        "config/dictionary.json",
        "core lexicon: lexemes, functional words",
    ),
    (
        "config/emotion_lexicon.json",
        "emotion words used by companion-theme extraction",
    ),
    (
        "config/relation_rules.json",
        "directed relation rules for corpus ingest",
    ),
    (
        "config/name_validation.json",
        "name validation rules for the compiler",
    ),
    ("config/faction_map.json", "faction map for corpus ingest"),
    (
        "config/persona_prototypes.json",
        "prototypes used by the persona_check tool",
    ),
    (
        "config/persona_cards.json",
        "persona cards used by the persona_inject tool",
    ),
    (
        "config/decay_config.json",
        "decay policy (absent = built-in defaults)",
    ),
    ("lexicon/packs", "lexicon packs directory"),
    (
        "lexicon/user.json",
        "your own lexicon overlay (merged last; disables via `disabled`)",
    ),
];

/// JSON resources the checker parses, so a malformed file is caught HERE rather
/// than silently degrading the first request that needs it (audit H16).
///
/// The marker tables are deliberately absent: `inspect_markers` already parses
/// them with richer bucket-level validation, so re-listing them would duplicate
/// work. Every other resource is checked for JSON well-formedness — the loader
/// level schema checks still run (and keep their own fallbacks) at first use.
const JSON_RESOURCES: &[&str] = &[
    "config/dictionary.json",
    "config/emotion_lexicon.json",
    "config/relation_rules.json",
    "config/name_validation.json",
    "config/faction_map.json",
    "config/persona_prototypes.json",
    "config/persona_cards.json",
    "config/decay_config.json",
    "lexicon/user.json",
];

/// The result of a configuration check.
#[derive(Debug, Clone)]
pub struct ConfigCheck {
    /// Printable lines, in order.
    pub lines: Vec<String>,
    /// Process exit code: 0 when the table can be trusted, 1 otherwise.
    pub exit_code: i32,
}

/// The exit code a report implies.
///
/// Only the marker table can fail the check: a missing optional resource is a
/// downgrade, while an unknown bucket or an unparsable marker file means the
/// engine would remember something other than what the file says.
#[must_use]
pub fn exit_code(report: &MarkerReport) -> i32 {
    i32::from(report.has_errors())
}

/// Check the configuration and return the report plus an exit code.
#[must_use]
pub fn config_check() -> ConfigCheck {
    config_check_at(&resolve_resource_path(""))
}

/// Check the configuration rooted at `root`.
///
/// Split from [`config_check`] so the resource-parsing behaviour is testable
/// against a fixture tree without touching process-global state.
#[must_use]
fn config_check_at(root: &Path) -> ConfigCheck {
    let report = inspect_markers();
    let failures = resource_parse_failures(root);
    let mut lines = marker_report_lines(report);
    if !failures.is_empty() {
        lines.push(String::new());
        lines.push("errors (fix these: a resource file is not valid JSON)".to_string());
        for (relative, reason) in &failures {
            lines.push(format!("  {relative}: {reason}"));
        }
    }
    ConfigCheck {
        lines,
        // A marker failure OR an unparsable resource fails the check.
        exit_code: exit_code(report).max(i32::from(!failures.is_empty())),
    }
}

/// Every `lexicon/packs/*.json` under `root`, sorted by name.
fn domain_pack_paths(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root.join("lexicon/packs")) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        .collect();
    paths.sort();
    paths
}

/// Parse every JSON resource under `root`; return `(relative path, reason)` for
/// the ones that EXIST but are not valid JSON.
///
/// Absence is never a failure: the engine has built-in fallbacks and a minimal
/// deployment may ship none of these files. The case the old checker missed is
/// a file that is present and corrupt — it used to surface only at first use.
fn resource_parse_failures(root: &Path) -> Vec<(String, String)> {
    let config_files = JSON_RESOURCES
        .iter()
        .map(|relative| ((*relative).to_string(), root.join(relative)));
    let pack_files = domain_pack_paths(root).into_iter().filter_map(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .map(|name| (format!("lexicon/packs/{name}"), path))
    });

    let mut failures = Vec::new();
    for (relative, path) in config_files.chain(pack_files) {
        // Unreadable-as-missing is treated like absent, matching the loaders.
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Err(error) = serde_json::from_str::<serde_json::Value>(&text) {
            failures.push((relative, error.to_string()));
        }
    }
    failures
}

/// Ensure the built-in vocabulary is usable, mapping a broken resource to a
/// startup error.
///
/// Called by `main` before it starts serving. A resource that EXISTS but cannot
/// be parsed is a hard error — an operator who broke the file must not think it
/// loaded (audit H18). An absent resource is fine, so a minimal deployment with
/// no `config/` still starts.
pub fn verify_vocabulary() -> Result<(), String> {
    crate::lexicon::try_init()?;
    crate::dictionary::try_init()?;
    Ok(())
}

/// Format a marker-table report as printable lines.
#[must_use]
pub fn marker_report_lines(report: &MarkerReport) -> Vec<String> {
    let mut lines = vec![
        format!("resource root: {}", resolve_resource_path("").display()),
        String::new(),
        "marker table".to_string(),
    ];
    for (path, words) in &report.per_file {
        lines.push(format!("  loaded   {} ({words} words)", short(path)));
    }
    for (path, reason) in &report.failed {
        lines.push(format!("  FAILED   {} ({reason})", short(path)));
    }
    if report.used_fallback {
        lines.push(
            "  fallback the built-in safety net is in use — no marker file could be read"
                .to_string(),
        );
    }
    lines.push(format!(
        "  effective: {} words across {} actions",
        report.total_words(),
        report.per_action.len()
    ));
    for (action, count) in &report.per_action {
        let doc = OBSERVATION_ACTIONS
            .iter()
            .find(|candidate| candidate.name == action)
            .map_or("", |candidate| candidate.doc);
        lines.push(format!("    {action:<10} {count:>4}  {doc}"));
    }

    let warnings = warning_lines(report);
    if !warnings.is_empty() {
        lines.push(String::new());
        lines.push("warnings (the table still loads)".to_string());
        lines.extend(warnings);
    }

    if report.has_errors() {
        lines.push(String::new());
        lines.push("errors (fix these: the table is not what the files say)".to_string());
        for (path, reason) in &report.failed {
            lines.push(format!("  {}: {reason}", short(path)));
        }
        let allowed = OBSERVATION_ACTIONS
            .iter()
            .map(|action| action.name)
            .collect::<Vec<_>>()
            .join(", ");
        for (action, samples) in &report.unknown_actions {
            lines.push(format!(
                "  `{action}` is not a documented action (words: {}); allowed: {allowed}",
                samples.join(", ")
            ));
        }
    }

    lines.push(String::new());
    lines.push("resource files".to_string());
    for (relative, purpose) in RESOURCE_FILES {
        let state = if relative.contains('*') {
            "glob"
        } else if resolve_resource_path(relative).exists() {
            "present"
        } else {
            "absent"
        };
        lines.push(format!("  {state:<7}  {relative:<34} {purpose}"));
    }
    lines
}

/// Warnings: things that load but are worth knowing about.
fn warning_lines(report: &MarkerReport) -> Vec<String> {
    let mut lines = Vec::new();
    for path in &report.missing {
        // The net only steps in when NO file can be read, so a single absent
        // file (say the English table on a Chinese-only deployment) really does
        // mean those words are no longer matched — worth saying out loud.
        lines.push(format!(
            "  {} is absent: its words are not in the effective table",
            short(path)
        ));
    }
    for (word, actions) in &report.cross_bucket {
        lines.push(format!(
            "  `{word}` appears under {} — one message yields one fact per action",
            actions.join(", ")
        ));
    }
    for note in &report.ignored {
        lines.push(format!("  ignored: {note}"));
    }
    for (action, word) in &report.unmatched_removals {
        lines.push(format!(
            "  `_remove.{{{action}}}` listed `{word}`, which is not in the table — \
             check the spelling of both"
        ));
    }
    for (action, word) in &report.removed {
        lines.push(format!("  removed `{word}` from `{action}`"));
    }
    if report.duplicates > 0 {
        lines.push(format!(
            "  collapsed {} duplicate entr{}",
            report.duplicates,
            if report.duplicates == 1 { "y" } else { "ies" }
        ));
    }
    lines
}

/// Show a path relative to the resource root when it lives under it.
fn short(path: &Path) -> String {
    let root = resolve_resource_path("");
    path.strip_prefix(&root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;

    /// Objective: Verify a broken marker table fails the check and names both
    /// the offending bucket and the allowed ones — the whole point of the
    /// command is that a typo cannot pass unnoticed.
    /// Invariants: exit code 1; the error section lists the bucket, its words
    /// and the documented actions.
    #[test]
    fn errors_are_reported_and_fail_the_check() {
        let report = MarkerReport {
            loaded: vec![PathBuf::from("config/markers_zh.json")],
            per_file: vec![(PathBuf::from("config/markers_zh.json"), 1)],
            failed: vec![(
                PathBuf::from("config/broken.user.json"),
                "invalid JSON: expected value".to_string(),
            )],
            per_action: BTreeMap::from([("feel".to_string(), 1)]),
            unknown_actions: BTreeMap::from([("feell".to_string(), vec!["emo".to_string()])]),
            ..MarkerReport::default()
        };

        assert_eq!(exit_code(&report), 1, "an error must fail the check");
        let text = marker_report_lines(&report).join("\n");
        assert!(
            text.contains("errors (fix these"),
            "an error section is required, got:\n{text}"
        );
        assert!(
            text.contains("`feell` is not a documented action (words: emo)"),
            "the unknown bucket must be named with its words, got:\n{text}"
        );
        assert!(
            text.contains("allowed: 喜欢"),
            "the allowed actions must be listed, got:\n{text}"
        );
        assert!(
            text.contains("config/broken.user.json"),
            "the unparsable file must be named, got:\n{text}"
        );
    }

    /// Objective: Verify the warnings are informative without failing the
    /// check: a cross-bucket word, a removal that matched nothing, an ignored
    /// entry and a duplicate are all reported, and the exit code stays 0.
    /// Invariants: exit code 0; each warning appears exactly where the user can
    /// act on it.
    #[test]
    fn warnings_do_not_fail_the_check() {
        let report = MarkerReport {
            loaded: vec![PathBuf::from("config/markers_zh.json")],
            per_file: vec![(PathBuf::from("config/markers_zh.json"), 3)],
            per_action: BTreeMap::from([("feel".to_string(), 3)]),
            cross_bucket: BTreeMap::from([(
                "下头".to_string(),
                vec!["dislike".to_string(), "feel".to_string()],
            )]),
            unmatched_removals: vec![("feel".to_string(), "应酬".to_string())],
            ignored: vec!["config/x.user.json: `want` has no usable words".to_string()],
            duplicates: 1,
            ..MarkerReport::default()
        };

        assert_eq!(exit_code(&report), 0, "warnings must not fail the check");
        let text = marker_report_lines(&report).join("\n");
        assert!(
            text.contains("`下头` appears under dislike, feel"),
            "a cross-bucket word must be reported, got:\n{text}"
        );
        assert!(
            text.contains("`_remove.{feel}` listed `应酬`"),
            "an unmatched removal must be reported, got:\n{text}"
        );
        assert!(
            text.contains("collapsed 1 duplicate entry"),
            "duplicates must be reported, got:\n{text}"
        );
        assert!(
            text.contains("ignored: config/x.user.json"),
            "ignored entries must be reported, got:\n{text}"
        );
        assert!(
            !text.contains("errors (fix these"),
            "warnings alone must not open an error section, got:\n{text}"
        );
    }

    /// Objective: Verify an absent marker file is warned about but does not fail
    /// the check: the built-in net only steps in when NO file can be read, so a
    /// single absent table silently removes vocabulary and must be visible.
    /// Invariants: exit code 0 and an explicit warning naming the file.
    #[test]
    fn missing_marker_file_warns_without_failing() {
        let report = MarkerReport {
            loaded: vec![PathBuf::from("config/markers_zh.json")],
            per_file: vec![(PathBuf::from("config/markers_zh.json"), 2)],
            missing: vec![PathBuf::from("config/markers_en.json")],
            per_action: BTreeMap::from([("feel".to_string(), 2)]),
            ..MarkerReport::default()
        };

        assert_eq!(
            exit_code(&report),
            0,
            "an absent optional file must not fail"
        );
        let text = marker_report_lines(&report).join("\n");
        assert!(
            text.contains("config/markers_en.json is absent"),
            "the absent file must be named, got:\n{text}"
        );
        assert!(
            text.contains("not in the effective table"),
            "the consequence must be stated, got:\n{text}"
        );
    }

    /// Objective: Verify the shipped configuration is self-consistent: this is
    /// the regression gate for `config/` itself. A shipped table that fell back
    /// to the net, or reused a bucket name the engine does not know, would ship
    /// silently wrong facts.
    /// Invariants: no fallback, no failures, no unknown buckets, non-empty.
    #[test]
    fn shipped_configuration_passes_the_check() {
        let report = inspect_markers();
        assert!(
            !report.used_fallback,
            "the shipped marker files must load, got {report:?}"
        );
        assert!(
            report.failed.is_empty(),
            "shipped marker files must parse, got {:?}",
            report.failed
        );
        assert!(
            report.unknown_actions.is_empty(),
            "shipped buckets must all be documented actions, got {:?}",
            report.unknown_actions
        );
        assert!(
            report.total_words() > 0,
            "the effective table must not be empty"
        );
        assert_eq!(
            exit_code(report),
            0,
            "the shipped configuration must pass its own check"
        );
    }

    /// Objective: Verify the resource listing answers "which files should
    /// exist?" with a concrete path list.
    /// Invariants: every listed marker file appears, and the listing names the
    /// user table convention.
    #[test]
    fn resource_listing_names_the_marker_files_and_user_convention() {
        let text = marker_report_lines(&MarkerReport::default()).join("\n");
        assert!(
            text.contains("config/markers_zh.json") && text.contains("config/markers_en.json"),
            "both shipped marker files must be listed, got:\n{text}"
        );
        assert!(
            text.contains("config/*.user.json"),
            "the user-table convention must be listed, got:\n{text}"
        );
        assert!(
            text.contains("resource files"),
            "the listing needs a heading, got:\n{text}"
        );
    }

    /// Objective: Verify a resource file that exists but is not valid JSON is
    /// reported by the checker and fails it. Before the fix only the marker
    /// tables were parsed, so a broken `dictionary.json` passed `config-check`
    /// and surfaced only at first use (audit H16).
    /// Invariants: exit code 1; the error section names the file and states it
    /// is not valid JSON.
    #[test]
    fn malformed_resource_is_reported_by_the_checker() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("config")).expect("create config dir");
        std::fs::write(root.join("config/dictionary.json"), "{ this is not json")
            .expect("write a broken resource");

        let check = config_check_at(root);
        assert_eq!(
            check.exit_code, 1,
            "an unparsable resource must fail the check"
        );
        let text = check.lines.join("\n");
        assert!(
            text.contains("is not valid JSON"),
            "the checker must open a JSON error section, got:\n{text}"
        );
        assert!(
            text.contains("config/dictionary.json"),
            "the offending file must be named, got:\n{text}"
        );
    }

    /// Objective: Verify the `lexicon/packs/*.json` overlay is parsed too, so a
    /// corrupt pack is caught here rather than by the lexicon loader at first
    /// use.
    /// Invariants: the broken pack is named in the failure list.
    #[test]
    fn malformed_pack_is_reported_by_the_checker() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("lexicon/packs")).expect("create packs dir");
        std::fs::write(root.join("lexicon/packs/broken.json"), "{ nope")
            .expect("write a broken pack");

        let failures = resource_parse_failures(root);
        assert!(
            failures
                .iter()
                .any(|(path, _)| path == "lexicon/packs/broken.json"),
            "a broken pack must be reported, got {failures:?}"
        );
    }

    /// Objective: Verify an absent resource is NOT reported: a minimal
    /// deployment with no `config/` directory must still pass the check.
    /// Invariants: an empty resource tree yields no failures.
    #[test]
    fn absent_resources_are_not_failures() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        assert!(
            resource_parse_failures(dir.path()).is_empty(),
            "an empty resource root must not report failures"
        );
    }

    /// Objective: Verify every shipped resource parses: this is the regression
    /// gate for `config/` and `lexicon/packs/` themselves, so a malformed
    /// shipped file cannot reach a release (audit H16).
    /// Invariants: no resource under the real root fails to parse.
    #[test]
    fn shipped_resources_all_parse() {
        let failures = resource_parse_failures(&resolve_resource_path(""));
        assert!(
            failures.is_empty(),
            "every shipped resource must be valid JSON, got {failures:?}"
        );
    }

    /// Objective: Verify the startup vocabulary check accepts the shipped
    /// config, so the new hard-failure wiring does not break the normal case.
    /// Invariants: `verify_vocabulary()` returns `Ok(())`.
    #[test]
    fn verify_vocabulary_accepts_the_shipped_config() {
        assert!(
            verify_vocabulary().is_ok(),
            "the shipped vocabulary must pass the startup check"
        );
    }
}
