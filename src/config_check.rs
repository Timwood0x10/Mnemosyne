//! `config-check`: report how the runtime resource files were loaded.
//!
//! The vocabulary lives in `config/`, so customising the engine means editing
//! data rather than code. This is the feedback loop for that: it prints which
//! files were read, what each one was worth, the effective word count per
//! action, and every entry that was dropped or ignored — then fails the process
//! when the table cannot be trusted.
//!
//! Read-only: it never rewrites a config file, and it never starts the server.

use std::path::Path;

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
        "config/anchor_seeds.json",
        "anchor seeds for value extraction",
    ),
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
    let report = inspect_markers();
    ConfigCheck {
        lines: marker_report_lines(report),
        exit_code: exit_code(report),
    }
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
}
