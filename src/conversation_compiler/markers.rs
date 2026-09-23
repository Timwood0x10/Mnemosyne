//! Observation-marker tables loaded from config, with built-in fallbacks.

use super::*;

/// Single-pass matcher over all functional lexemes (negation, uncertainty, …).
///
/// Built once per process from the lexicon registry (P3: never rebuild inside
/// the per-message loop). The Aho-Corasick automaton scans each message in one
/// pass instead of O(V) `contains` checks per class.
pub(super) static FUNCTIONAL_MATCHER: LazyLock<crate::lexicon::LexiconMatcher> =
    LazyLock::new(crate::lexicon::LexiconMatcher::from_global_registry);

/// Observation action markers: `{"feel": ["开心", "疲惫", …], "plan": […], …}`.
///
/// Loaded once from the two shipped files — `config/markers_zh.json`
/// (Chinese) and `config/markers_en.json` (English) — which ship with the
/// binary so users can customize the vocabulary without recompiling. Each
/// file that loads successfully contributes its markers; if BOTH are missing
/// or corrupt the loader degrades to [`DEFAULT_OBSERVATION_MARKERS`] so a
/// config-less deployment keeps compiling facts (same fail-soft pattern as
/// the lexicon and dictionary loaders).
pub(super) static OBSERVATION_MARKERS: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    let zh = crate::config::resolve_resource_path("config/markers_zh.json");
    let en = crate::config::resolve_resource_path("config/markers_en.json");
    merge_marker_files(&[zh, en])
});

/// Merge the marker files (each `{"action": ["marker", …]}`) into flat
/// `(marker, action)` pairs. Every file that parses contributes its markers;
/// when NONE of them yields any marker, the built-in default table is used so
/// a config-less deployment keeps compiling facts.
///
/// The `_meta` documentation key (an object, not a marker list) is skipped —
/// it exists so users can read the file's purpose in a JSON viewer.
pub(super) fn merge_marker_files(paths: &[std::path::PathBuf]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut loaded_any = false;
    for path in paths {
        let Some(raw) = std::fs::read_to_string(path).ok() else {
            eprintln!("warning: {} failed to load; skipping", path.display());
            continue;
        };
        // Parse as Value and walk keys manually so the `_meta` documentation
        // object does not fail the whole file (it is not a marker list).
        let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&raw)
        else {
            eprintln!("warning: {} failed to parse; skipping", path.display());
            continue;
        };
        for (action, markers) in map {
            if action == "_meta" {
                continue;
            }
            let serde_json::Value::Array(items) = markers else {
                continue;
            };
            for marker in items {
                if let Some(m) = marker.as_str() {
                    if !m.is_empty() {
                        pairs.push((m.to_string(), action.clone()));
                    }
                }
            }
        }
        loaded_any = true;
    }
    if loaded_any && !pairs.is_empty() {
        pairs
    } else {
        eprintln!("warning: no observation marker config loaded; using built-in defaults");
        default_markers_owned()
    }
}

/// Convert the built-in fallback table into owned `(String, String)` pairs.
fn default_markers_owned() -> Vec<(String, String)> {
    DEFAULT_OBSERVATION_MARKERS
        .iter()
        .map(|(m, a)| (m.to_string(), a.to_string()))
        .collect()
}

/// Built-in fallback markers, used when the JSON config is missing/corrupt.
/// Mirrors the shipped `config/markers_zh.json` + `config/markers_en.json`.
const DEFAULT_OBSERVATION_MARKERS: &[(&str, &str)] = &[
    // 偏好正向
    ("喜欢", "喜欢"),
    ("偏好", "喜欢"),
    ("欣赏", "喜欢"),
    ("羡慕", "喜欢"),
    ("热爱", "喜欢"),
    ("满意", "喜欢"),
    ("认可", "喜欢"),
    ("佩服", "喜欢"),
    ("崇拜", "喜欢"),
    ("感恩", "喜欢"),
    ("讨厌", "喜欢"),
    ("love", "love"),
    ("like", "like"),
    // 目标 / 计划
    ("准备", "准备"),
    ("打算", "plan"),
    ("计划", "plan"),
    ("希望", "plan"),
    ("目标", "plan"),
    ("梦想", "plan"),
    ("志向", "plan"),
    ("决心", "plan"),
    // 意愿（防单字"想"误匹配）
    ("想要", "want"),
    ("想学", "want"),
    ("想买", "want"),
    ("想换", "want"),
    ("想找", "want"),
    ("想去", "want"),
    ("想见", "want"),
    ("want", "want"),
    // 情绪正向
    ("开心", "feel"),
    ("高兴", "feel"),
    ("快乐", "feel"),
    ("幸福", "feel"),
    ("欣慰", "feel"),
    ("安心", "feel"),
    ("满足", "feel"),
    ("轻松", "feel"),
    ("愉快", "feel"),
    ("兴奋", "feel"),
    ("舒服", "feel"),
    ("自在", "feel"),
    ("喜悦", "feel"),
    // 情绪负向
    ("难过", "feel"),
    ("伤心", "feel"),
    ("失落", "feel"),
    ("沮丧", "feel"),
    ("崩溃", "feel"),
    ("紧张", "feel"),
    ("压抑", "feel"),
    ("无奈", "feel"),
    ("迷茫", "feel"),
    ("无助", "feel"),
    ("绝望", "feel"),
    ("心累", "feel"),
    ("孤单", "feel"),
    ("孤独", "feel"),
    ("愤怒", "feel"),
    ("失望", "feel"),
    ("痛苦", "feel"),
    ("烦恼", "feel"),
    ("委屈", "feel"),
    ("烦躁", "feel"),
    ("郁闷", "feel"),
    ("生气", "feel"),
    ("心疼", "feel"),
    // 状态 / 身体感受
    ("疲惫", "feel"),
    ("好累", "feel"),
    ("太累", "feel"),
    ("困倦", "feel"),
    ("生病", "feel"),
    ("感冒", "feel"),
    ("发烧", "feel"),
    ("头疼", "feel"),
    ("头晕", "feel"),
    ("失眠", "feel"),
    ("熬夜", "feel"),
    ("加班", "feel"),
    ("应酬", "feel"),
    ("压力", "feel"),
    ("焦虑", "feel"),
    ("担心", "feel"),
    ("害怕", "feel"),
    ("stress", "feel"),
    ("tired", "feel"),
    ("happy", "feel"),
    ("sad", "feel"),
    ("worry", "feel"),
    ("afraid", "feel"),
];
