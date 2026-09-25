//! Observation-marker tables loaded from config, with a built-in safety net.
//!
//! ## Vocabulary contract
//!
//! The **words** belong to the user; the **bucket names** belong to the engine,
//! because each bucket decides a fact type. A bucket must therefore be one of
//! [`OBSERVATION_ACTIONS`](crate::observation_compiler::OBSERVATION_ACTIONS).
//! An unknown bucket is dropped and reported (see [`MarkerReport`]): it used to
//! fall through to `Event` facts silently, so a typo looked like a working
//! config that remembered the wrong thing.
//!
//! ## Load order
//!
//! 1. `config/markers_zh.json` — shipped reference table
//! 2. `config/markers_en.json` — shipped reference table
//! 3. `config/*.user.json` — the user's own tables, merged by file name
//!
//! Upgrades overwrite the shipped files, so customisation belongs in a
//! `*.user.json` file. Every file may carry a `_remove` object
//! (`{"_remove": {"feel": ["应酬"]}}`) which deletes already-merged words — the
//! only way to switch off a shipped default that misfires in a domain. Within
//! one file removals are applied after that file's additions, so a word a file
//! both adds and removes ends up removed; a later file can add it back.
//!
//! ## Fallback
//!
//! When no file yields any entry the built-in net in
//! [`DEFAULT_OBSERVATION_MARKERS`] is used. It is deliberately small — a net,
//! not a second copy of the shipped tables, which is how the two silently
//! drifted apart before.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use crate::observation_compiler::action_fact_type;

/// Single-pass matcher over all functional lexemes (negation, uncertainty, …).
///
/// Built once per process from the lexicon registry (P3: never rebuild inside
/// the per-message loop). The Aho-Corasick automaton scans each message in one
/// pass instead of O(V) `contains` checks per class.
pub(super) static FUNCTIONAL_MATCHER: LazyLock<crate::lexicon::LexiconMatcher> =
    LazyLock::new(crate::lexicon::LexiconMatcher::from_global_registry);

/// Observation action markers: the effective table plus its diagnostics.
///
/// Loaded once per process from `config/` (see the module docs for the order),
/// so users can customize the vocabulary without recompiling.
pub(super) static OBSERVATION_MARKERS: LazyLock<MarkerTable> = LazyLock::new(|| {
    let (pairs, report) = merge_with_report(&marker_file_paths());
    MarkerTable { pairs, report }
});

/// The effective marker table and how it was assembled.
pub(super) struct MarkerTable {
    /// Flat `(marker, action)` pairs the compiler matches against.
    pub(super) pairs: Vec<(String, String)>,
    /// How the table was built, for the self-check.
    pub(super) report: MarkerReport,
}

/// The effective table's diagnostics, for `mnemosyne config-check`.
#[must_use]
pub fn inspect_markers() -> &'static MarkerReport {
    &OBSERVATION_MARKERS.report
}

/// How the effective marker table was assembled.
///
/// Every field exists to be *shown to a user*: the table lives in `config/`, so
/// a mistake must be visible instead of quietly changing what is remembered.
#[derive(Debug, Default, Clone)]
pub struct MarkerReport {
    /// Files that were read and parsed.
    pub loaded: Vec<PathBuf>,
    /// Words each loaded file contributed to the effective table (after
    /// validation, before later files' removals).
    pub per_file: Vec<(PathBuf, usize)>,
    /// Files that were referenced but absent.
    pub missing: Vec<PathBuf>,
    /// Files that exist but could not be read or parsed, with the reason.
    pub failed: Vec<(PathBuf, String)>,
    /// Effective word count per action, after removals.
    pub per_action: BTreeMap<String, usize>,
    /// Words deleted by a `_remove` section.
    pub removed: Vec<(String, String)>,
    /// `_remove` entries that matched nothing (a typo in the word or bucket).
    pub unmatched_removals: Vec<(String, String)>,
    /// Buckets outside the action vocabulary, with sample words.
    pub unknown_actions: BTreeMap<String, Vec<String>>,
    /// Entries dropped with a reason (empty word, non-string entry, …).
    pub ignored: Vec<String>,
    /// Words present under more than one action: a message matching one of them
    /// yields one fact per action.
    pub cross_bucket: BTreeMap<String, Vec<String>>,
    /// Exact duplicates collapsed inside one bucket.
    pub duplicates: usize,
    /// True when no file yielded any entry and the built-in net was used.
    pub used_fallback: bool,
}

impl MarkerReport {
    /// True when the table must not be trusted to mean what it says.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        !self.failed.is_empty() || !self.unknown_actions.is_empty()
    }

    /// Total number of effective words.
    #[must_use]
    pub fn total_words(&self) -> usize {
        self.per_action.values().sum()
    }
}

/// A parsed marker file: additions plus its `_remove` section.
#[derive(Debug, Default)]
struct ParsedMarkers {
    additions: Vec<(String, Vec<String>)>,
    removals: Vec<(String, Vec<String>)>,
    problems: Vec<String>,
}

/// Parse `{"action": ["word", …], "_remove": {"action": ["word", …]}}`.
///
/// # Errors
///
/// Returns the reason when the file is not a JSON object, or when a bucket is
/// not a list of words — a whole-file failure, unlike a single unusable entry,
/// which is recorded in [`ParsedMarkers::problems`] and dropped.
fn parse_marker_file(raw: &str) -> std::result::Result<ParsedMarkers, String> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|error| format!("invalid JSON: {error}"))?;
    let serde_json::Value::Object(map) = value else {
        return Err("top level must be an object of action → word list".to_string());
    };
    let mut parsed = ParsedMarkers::default();
    for (key, value) in map {
        // `_meta` documents the file for humans; it is not a word list.
        if key == "_meta" {
            continue;
        }
        if key == "_remove" {
            let serde_json::Value::Object(removals) = value else {
                parsed
                    .problems
                    .push("`_remove` must be an object of action → word list".to_string());
                continue;
            };
            for (action, words) in removals {
                let words = strings_of(&words, &mut parsed.problems);
                parsed.removals.push((action, words));
            }
            continue;
        }
        let words = strings_of(&value, &mut parsed.problems);
        if words.is_empty() {
            parsed.problems.push(format!(
                "`{key}` has no usable words (use `_remove` to delete existing ones)"
            ));
        }
        parsed.additions.push((key, words));
    }
    Ok(parsed)
}

/// Coerce a word list, recording anything unusable.
fn strings_of(value: &serde_json::Value, problems: &mut Vec<String>) -> Vec<String> {
    let serde_json::Value::Array(items) = value else {
        problems.push("word lists must be JSON arrays".to_string());
        return Vec::new();
    };
    let mut words = Vec::with_capacity(items.len());
    for item in items {
        match item.as_str() {
            Some(word) => words.push(word.to_string()),
            None => problems.push(format!("`{item}` is not a string")),
        }
    }
    words
}

/// Merge marker files into the effective table, validating as it goes.
///
/// The single implementation behind both the runtime table and the tests.
fn merge_with_report(paths: &[PathBuf]) -> (Vec<(String, String)>, MarkerReport) {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut report = MarkerReport::default();
    let mut parsed_any = false;

    for path in paths {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                report.missing.push(path.clone());
                continue;
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "marker config unreadable; skipping");
                report.failed.push((path.clone(), error.to_string()));
                continue;
            }
        };
        let parsed = match parse_marker_file(&raw) {
            Ok(parsed) => parsed,
            Err(reason) => {
                tracing::warn!(path = %path.display(), reason = %reason, "marker config rejected; skipping");
                report.failed.push((path.clone(), reason));
                continue;
            }
        };
        parsed_any = true;
        report.loaded.push(path.clone());
        for problem in parsed.problems {
            tracing::warn!(path = %path.display(), problem = %problem, "marker config entry ignored");
            report
                .ignored
                .push(format!("{}: {problem}", path.display()));
        }
        // Additions first, removals second: a word one file both adds and
        // removes ends up removed, which is how a `_remove` section reads.
        let mut contributed = 0usize;
        for (action, words) in parsed.additions {
            contributed += absorb(&mut pairs, &mut report, &action, words);
        }
        report.per_file.push((path.clone(), contributed));
        for (action, words) in parsed.removals {
            remove_words(&mut pairs, &mut report, &action, words);
        }
    }

    if pairs.is_empty() && !parsed_any {
        tracing::warn!("no observation marker config loaded; using the built-in safety net");
        report.used_fallback = true;
        pairs = default_markers_owned();
    }

    report.per_action = pairs
        .iter()
        .fold(BTreeMap::new(), |mut counts, (_, action)| {
            *counts.entry(action.clone()).or_insert(0) += 1;
            counts
        });
    report.cross_bucket = cross_bucket_words(&pairs);
    (pairs, report)
}

/// Absorb one bucket's words, refusing a bucket outside the vocabulary.
///
/// Returns how many words the bucket actually contributed, so the report can
/// say what each file was worth.
fn absorb(
    pairs: &mut Vec<(String, String)>,
    report: &mut MarkerReport,
    action: &str,
    words: Vec<String>,
) -> usize {
    let known = action_fact_type(action).is_some();
    let mut contributed = 0usize;
    for word in words {
        let word = word.trim();
        if word.is_empty() {
            report.ignored.push(format!("empty word under `{action}`"));
            continue;
        }
        if !known {
            note_unknown(report, action, word);
            continue;
        }
        let entry = (word.to_string(), action.to_string());
        if pairs.contains(&entry) {
            report.duplicates += 1;
            continue;
        }
        pairs.push(entry);
        contributed += 1;
    }
    contributed
}

/// Delete `words` from `action` in the merged table, reporting what happened.
///
/// A removal that matches nothing is reported instead of being ignored: the
/// usual cause is a typo in the word or in the bucket name, and silently doing
/// nothing would look exactly like a working `_remove`.
fn remove_words(
    pairs: &mut Vec<(String, String)>,
    report: &mut MarkerReport,
    action: &str,
    words: Vec<String>,
) {
    if action_fact_type(action).is_none() {
        for word in words {
            note_unknown(report, action, word.trim());
        }
        return;
    }
    for word in words {
        let word = word.trim().to_string();
        if word.is_empty() {
            report
                .ignored
                .push(format!("empty word under `_remove.{action}`"));
            continue;
        }
        let before = pairs.len();
        pairs.retain(|(existing, existing_action)| {
            !(existing == &word && existing_action == action)
        });
        if pairs.len() == before {
            report.unmatched_removals.push((action.to_string(), word));
        } else {
            report.removed.push((action.to_string(), word));
        }
    }
}

/// Record a bucket outside the vocabulary, keeping a few sample words.
fn note_unknown(report: &mut MarkerReport, action: &str, word: &str) {
    let samples = report
        .unknown_actions
        .entry(action.to_string())
        .or_default();
    if samples.len() < 3 {
        samples.push(word.to_string());
    }
}

/// Words that appear under more than one action.
///
/// Actions are sorted so the report does not depend on the JSON key order
/// (serde_json only preserves it behind a feature flag).
fn cross_bucket_words(pairs: &[(String, String)]) -> BTreeMap<String, Vec<String>> {
    let mut by_word: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (word, action) in pairs {
        by_word
            .entry(word.as_str())
            .or_default()
            .push(action.as_str());
    }
    by_word
        .into_iter()
        .filter(|(_, actions)| actions.len() > 1)
        .map(|(word, mut actions)| {
            actions.sort_unstable();
            (
                word.to_string(),
                actions.into_iter().map(str::to_string).collect(),
            )
        })
        .collect()
}

/// Every marker file to load, in order.
fn marker_file_paths() -> Vec<PathBuf> {
    let mut paths = vec![
        crate::config::resolve_resource_path("config/markers_zh.json"),
        crate::config::resolve_resource_path("config/markers_en.json"),
    ];
    paths.extend(user_marker_files());
    paths
}

/// The user's own tables: `config/*.user.json`, sorted by file name so the
/// merge is deterministic (a later file's `_remove` wins over earlier words).
fn user_marker_files() -> Vec<PathBuf> {
    user_tables_in(&crate::config::resolve_resource_path("config"))
}

/// The `*.user.json` files inside `dir`, sorted by name.
///
/// A missing or unreadable directory yields no files: the user simply has no
/// tables of their own, which is the common case.
fn user_tables_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            // `read_dir` also yields directories: a directory named
            // `something.user.json` is not a table and must not be read (it
            // would fail as an unreadable file and pollute the report).
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".user.json"))
        })
        .collect();
    files.sort();
    files
}

/// Merge explicit marker files into flat `(marker, action)` pairs.
///
/// Thin wrapper over the runtime merge so the tests drive the real logic
/// instead of a second implementation that can drift from it.
#[cfg(test)]
pub(super) fn merge_marker_files(paths: &[PathBuf]) -> Vec<(String, String)> {
    merge_with_report(paths).0
}

/// Convert the built-in fallback table into owned `(String, String)` pairs.
fn default_markers_owned() -> Vec<(String, String)> {
    DEFAULT_OBSERVATION_MARKERS
        .iter()
        .map(|(m, a)| (m.to_string(), a.to_string()))
        .collect()
}

/// Built-in safety net, used only when NO marker file could be parsed.
///
/// Deliberately **small**: a net, not a copy of the shipped reference tables.
/// The previous full copy drifted from the files it claimed to mirror (91 words
/// against their 420, and none of the `belief` / `stuck` / `life_event`
/// buckets), so a config-less deployment quietly produced a fraction of the
/// facts. `builtin_net_stays_inside_the_shipped_vocabulary` keeps it a subset
/// of the shipped words, so it can never invent vocabulary of its own.
///
/// Every documented action appears at least once, so the mapping path stays
/// exercised even without `config/`.
const DEFAULT_OBSERVATION_MARKERS: &[(&str, &str)] = &[
    // preference — 讨厌/反感 belong to the NEGATIVE bucket, not 喜欢: mapping
    // them to the positive action compiled "我讨厌应酬" as a positive stance.
    ("喜欢", "喜欢"),
    ("热爱", "喜欢"),
    ("讨厌", "dislike"),
    ("反感", "dislike"),
    ("love", "love"),
    ("like", "like"),
    ("dislike", "dislike"),
    // goal / plan / want
    ("准备", "准备"),
    ("打算", "plan"),
    ("计划", "plan"),
    ("plan", "plan"),
    ("想要", "want"),
    ("想学", "want"),
    ("want", "want"),
    // emotion / body state
    ("开心", "feel"),
    ("难过", "feel"),
    ("焦虑", "feel"),
    ("压力", "feel"),
    ("疲惫", "feel"),
    ("好累", "feel"),
    ("happy", "feel"),
    ("sad", "feel"),
    ("stress", "feel"),
    // belief / stuck / life_event — one representative word each so that no
    // documented action disappears when the config is missing.
    ("觉得", "belief"),
    ("卡住", "stuck"),
    ("搬家", "life_event"),
];

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::observation_compiler::OBSERVATION_ACTIONS;

    /// Write `raw` to `name` inside a temp dir and return the path.
    fn write(dir: &tempfile::TempDir, name: &str, raw: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, raw).expect("write marker file");
        path
    }

    /// The shipped marker file at `config/<name>`, read from the crate root.
    fn shipped(name: &str) -> serde_json::Value {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("config")
            .join(name);
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        serde_json::from_str(&raw)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
    }

    /// Objective: Verify a mistyped bucket name can no longer be mistaken for a
    /// working config. It used to be accepted and compiled into `Event` facts,
    /// so `feell` produced events where the user asked for emotions.
    /// Invariants: the words are dropped, the bucket is reported as unknown,
    /// and the report is flagged as an error.
    #[test]
    fn unknown_bucket_is_reported_and_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write(&dir, "typo.json", r#"{"feell": ["emo"], "feel": ["疲惫"]}"#);

        let (pairs, report) = merge_with_report(&[path]);
        assert_eq!(
            pairs,
            vec![("疲惫".to_string(), "feel".to_string())],
            "only the known bucket survives, got {pairs:?}"
        );
        assert_eq!(
            report.unknown_actions.get("feell"),
            Some(&vec!["emo".to_string()]),
            "the typo must be reported with its words"
        );
        assert!(report.has_errors(), "an unknown bucket is an error");
        assert!(
            !report.used_fallback,
            "a parsed file suppresses the fallback"
        );
    }

    /// Objective: Verify a user table may switch off a shipped default. Without
    /// removal the only way to silence a misfiring default word was editing the
    /// shipped file, which the next upgrade overwrites.
    /// Invariants: the word is gone, the neighbouring word stays, and the
    /// removal is reported.
    #[test]
    fn user_file_can_remove_a_shipped_word() {
        let dir = tempfile::tempdir().expect("tempdir");
        let shipped_path = write(&dir, "markers_zh.json", r#"{"feel": ["应酬", "疲惫"]}"#);
        let user_path = write(
            &dir,
            "markers_zh.user.json",
            r#"{"_remove": {"feel": ["应酬"]}, "feel": ["emo"]}"#,
        );

        let (pairs, report) = merge_with_report(&[shipped_path, user_path]);
        assert!(
            !pairs.contains(&("应酬".to_string(), "feel".to_string())),
            "the removed default must be gone, got {pairs:?}"
        );
        assert!(
            pairs.contains(&("疲惫".to_string(), "feel".to_string())),
            "an untouched default stays, got {pairs:?}"
        );
        assert!(
            pairs.contains(&("emo".to_string(), "feel".to_string())),
            "the user's own word is added, got {pairs:?}"
        );
        assert_eq!(
            report.removed,
            vec![("feel".to_string(), "应酬".to_string())],
            "a matched removal is reported"
        );
        assert!(
            report.unmatched_removals.is_empty(),
            "a matched removal is not unmatched"
        );
    }

    /// Objective: Verify a `_remove` that matches nothing is reported, because
    /// a typo there would otherwise look exactly like a successful removal.
    /// Invariants: the table is unchanged and the entry is listed as unmatched.
    #[test]
    fn unmatched_removal_is_reported_not_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write(
            &dir,
            "user.json",
            r#"{"feel": ["疲惫"], "_remove": {"feel": ["应酬"], "feell": ["x"]}}"#,
        );

        let (pairs, report) = merge_with_report(&[path]);
        assert_eq!(pairs.len(), 1, "the table is untouched, got {pairs:?}");
        assert_eq!(
            report.unmatched_removals,
            vec![("feel".to_string(), "应酬".to_string())],
            "a removal that matched nothing must be listed"
        );
        assert!(
            report.unknown_actions.contains_key("feell"),
            "an unknown bucket inside `_remove` is reported too, got {:?}",
            report.unknown_actions
        );
    }

    /// Objective: Verify a word that lives under two buckets is surfaced. Such a
    /// word yields one fact per action for a single message ("下头" produced both
    /// a preference and an emotion), which is worth knowing before it is
    /// mistaken for a compiler bug.
    /// Invariants: both actions are reported for the word.
    #[test]
    fn cross_bucket_words_are_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write(
            &dir,
            "user.json",
            r#"{"feel": ["下头"], "dislike": ["下头"], "feel_only": ["x"]}"#,
        );

        let (_, report) = merge_with_report(&[path]);
        assert_eq!(
            report.cross_bucket.get("下头"),
            Some(&vec!["dislike".to_string(), "feel".to_string()]),
            "both buckets listed, got {:?}",
            report.cross_bucket
        );
        assert!(
            !report.cross_bucket.contains_key("x"),
            "an unknown bucket contributes no cross-bucket word"
        );
    }

    /// Objective: Verify unusable entries are dropped with a reason instead of
    /// silently shrinking the table: an empty bucket list, a non-string entry.
    /// Invariants: the usable word survives and the two problems are recorded.
    #[test]
    fn unusable_entries_are_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write(
            &dir,
            "user.json",
            r#"{"feel": ["疲惫", 7], "want": [], "_remove": {"feel": []}}"#,
        );

        let (pairs, report) = merge_with_report(&[path]);
        assert_eq!(
            pairs,
            vec![("疲惫".to_string(), "feel".to_string())],
            "the usable word survives, got {pairs:?}"
        );
        assert_eq!(report.ignored.len(), 2, "got {:?}", report.ignored);
        assert!(
            report.ignored.iter().any(|note| note.contains("`want`")),
            "an empty bucket list is reported, got {:?}",
            report.ignored
        );
    }

    /// Objective: Verify the built-in net cannot drift from the shipped tables
    /// or invent vocabulary: every net word must exist in a shipped file, under
    /// the same action, and every action must be a documented action.
    /// Invariants: net ⊆ (markers_zh.json ∪ markers_en.json), per action.
    #[test]
    fn builtin_net_stays_inside_the_shipped_vocabulary() {
        let mut shipped_pairs: Vec<(String, String)> = Vec::new();
        for name in ["markers_zh.json", "markers_en.json"] {
            let value = shipped(name);
            for (action, words) in value.as_object().expect("object") {
                if action == "_meta" {
                    continue;
                }
                for word in words.as_array().expect("word list") {
                    shipped_pairs.push((
                        word.as_str().expect("string word").to_string(),
                        action.clone(),
                    ));
                }
            }
        }
        assert!(
            !shipped_pairs.is_empty(),
            "the shipped tables must not be empty"
        );
        for (word, action) in DEFAULT_OBSERVATION_MARKERS {
            assert!(
                action_fact_type(action).is_some(),
                "the net uses `{action}`, which is not a documented action"
            );
            assert!(
                shipped_pairs.contains(&(word.to_string(), action.to_string())),
                "`{word}`/`{action}` is not in the shipped tables, so the net would \
                 produce a fact the reference vocabulary cannot"
            );
        }
    }

    /// Objective: Verify every bucket the shipped files actually use is a
    /// documented action, so a shipped table can never rely on the silent
    /// `Event` fallback.
    /// Invariants: every bucket name resolves through `action_fact_type`.
    #[test]
    fn every_shipped_bucket_is_a_documented_action() {
        for name in ["markers_zh.json", "markers_en.json"] {
            let value = shipped(name);
            for action in value.as_object().expect("object").keys() {
                if action == "_meta" {
                    continue;
                }
                assert!(
                    action_fact_type(action).is_some(),
                    "{name} uses bucket `{action}`, which no action maps to; \
                     documented actions: {:?}",
                    OBSERVATION_ACTIONS
                        .iter()
                        .map(|action| action.name)
                        .collect::<Vec<_>>()
                );
            }
        }
    }

    /// Objective: Verify the user-table discovery reads exactly `*.user.json`,
    /// in name order, and treats an absent directory as "no user tables" rather
    /// than an error — a deployment with no customisation must keep working.
    /// Invariants: only `.user.json` files, sorted, and an absent directory
    /// yields an empty list.
    #[test]
    fn user_tables_are_globs_and_sorted() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir, "markers_zh.json", r#"{"feel": ["疲惫"]}"#);
        write(&dir, "b.user.json", r#"{"feel": ["b"]}"#);
        write(&dir, "a.user.json", r#"{"feel": ["a"]}"#);
        write(&dir, "notes.json", r#"{"feel": ["n"]}"#);
        std::fs::create_dir(dir.path().join("nested.user.json")).expect("create directory");

        let names: Vec<String> = user_tables_in(dir.path())
            .iter()
            .map(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .expect("utf-8 file name")
                    .to_string()
            })
            .collect();
        assert_eq!(
            names,
            vec!["a.user.json".to_string(), "b.user.json".to_string()],
            "only `*.user.json` files, sorted by name"
        );
        assert!(
            user_tables_in(&dir.path().join("does-not-exist")).is_empty(),
            "an absent config directory means no user tables"
        );
    }

    /// Objective: Verify the merge order is what the docs promise: a later user
    /// file can add back a word an earlier file removed, so the last word on a
    /// question wins.
    /// Invariants: the word is present at the end and both operations are
    /// reported.
    #[test]
    fn a_later_user_file_can_restore_a_removed_word() {
        let dir = tempfile::tempdir().expect("tempdir");
        let shipped_path = write(&dir, "markers_zh.json", r#"{"feel": ["应酬"]}"#);
        let first = write(&dir, "a.user.json", r#"{"_remove": {"feel": ["应酬"]}}"#);
        let second = write(&dir, "b.user.json", r#"{"feel": ["应酬"]}"#);

        let (pairs, report) = merge_with_report(&[shipped_path, first, second]);
        assert!(
            pairs.contains(&("应酬".to_string(), "feel".to_string())),
            "the last file wins, got {pairs:?}"
        );
        assert_eq!(
            report.removed,
            vec![("feel".to_string(), "应酬".to_string())],
            "the intermediate removal is still reported"
        );
    }

    /// Objective: Verify each documented action still maps to a fact type and no
    /// two rows share a name, so the table is a usable single source.
    /// Invariants: names are unique and every lookup succeeds.
    #[test]
    fn action_table_is_unique_and_complete() {
        let mut names: Vec<&str> = OBSERVATION_ACTIONS
            .iter()
            .map(|action| action.name)
            .collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "action names must be unique");
        for action in OBSERVATION_ACTIONS {
            assert_eq!(
                action_fact_type(action.name),
                Some(action.fact_type),
                "`{}` must resolve to its own fact type",
                action.name
            );
            assert!(
                !action.doc.is_empty(),
                "`{}` needs a one-line meaning for the user-facing docs",
                action.name
            );
        }
    }
}
