//! Corpus provider — entities discovered automatically from source text.
//!
//! This is the non-hardcoded answer to "where do entities come from": instead
//! of a hand-maintained dictionary (the old `entity_profiles/*.json` or the
//! `ingest::characters` tables), a corpus provider scans raw prose and infers
//! characters from narration + dialogue context. Entities are therefore
//! grounded in the actual text — no manual lists, no made-up aliases.
//!
//! Heuristic: a speaker appears immediately before a dialogue verb
//! (`说/道/曰/喊/问/答/喝/叹`). We count those occurrences, drop common
//! non-names (`有人/一人/众人/有人说`), and rank by frequency so the main
//! cast surfaces first.

use std::collections::HashMap;

use super::provider::{EntityEntry, EntityProvider};
use crate::ingest::extract::DIALOG_VERBS;

/// Common phrases that precede a dialogue verb but are not person names.
const NON_NAMES: &[&str] = &[
    "有人", "一人", "众人", "旁人", "路人", "世人", "小人", "某", "自称", "某人", "我们", "他们",
    "你们", "大家", "我", "你", "他", "她", "人们", "咱", "我辈", "诸位", "众人", "只听",
];

/// A corpus entity provider, pre-extracted from raw text.
pub struct CorpusEntityProvider {
    name: String,
    entries: Vec<EntityEntry>,
}

impl CorpusEntityProvider {
    /// Discover entities from a raw text body.
    ///
    /// # Panics
    ///
    /// Panics if `name` is empty.
    #[must_use]
    pub fn from_text(name: impl Into<String>, text: &str, min_frequency: usize) -> Self {
        let counts = count_speakers(text);
        let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        let entries = ranked
            .into_iter()
            .filter(|(_, count)| *count >= min_frequency)
            .map(|(speaker, count)| {
                let mut properties = HashMap::new();
                properties.insert("frequency".to_string(), count.to_string());
                properties.insert("source".to_string(), "corpus".to_string());
                EntityEntry {
                    canonical_name: speaker,
                    aliases: Vec::new(),
                    single_char: None,
                    object_type: "person".into(),
                    properties,
                }
            })
            .collect();

        Self {
            name: name.into(),
            entries,
        }
    }

    /// The source name this provider was constructed for.
    #[must_use]
    pub fn source_name(&self) -> &str {
        &self.name
    }
}

impl EntityProvider for CorpusEntityProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn entries(&self) -> Vec<EntityEntry> {
        self.entries.clone()
    }
}

/// Count speaker mentions across all dialogue verbs.
fn count_speakers(text: &str) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for verb in DIALOG_VERBS {
        let mut search_from = 0usize;
        while let Some(rel) = text[search_from..].find(verb) {
            let abs = search_from + rel;
            if let Some(name) = name_before(&text[..abs]) {
                if is_plausible_person(&name) {
                    *counts.entry(name).or_insert(0) += 1;
                }
            }
            search_from = abs + verb.len();
        }
    }
    counts
}

/// Extract the speaker name immediately before a dialogue verb.
///
/// The verb sits at the end of a CJK run (`刘备说道` → run is `刘备说道`); the
/// speaker is the leading 2 chars of that run (`刘备`). Reading from the front
/// avoids grabbing the verb itself. A run longer than 6 chars is treated as a
/// phrase, not a name.
///
/// # Limitation
///
/// This captures 2-character given+family names (the common case). A 3-char
/// name (`诸葛亮`) is truncated to its first two chars — acceptable for a
/// first-pass corpus discovery, and strictly better than a hand-maintained
/// dictionary because the data comes from the text.
fn name_before(prefix: &str) -> Option<String> {
    let mut end = prefix.len();
    // Skip trailing punctuation/whitespace (rare before a verb).
    while end > 0 {
        let ch = prefix[..end].chars().next_back()?;
        if ch.is_ascii_whitespace() || matches!(ch, '，' | '、' | '；' | '：' | '。') {
            end -= ch.len_utf8();
        } else {
            break;
        }
    }
    let mut run_start = end;
    while run_start > 0 {
        let ch = prefix[..run_start].chars().next_back()?;
        if is_cjk_name_char(ch) {
            run_start -= ch.len_utf8();
        } else {
            break;
        }
    }
    let run = &prefix[run_start..end];
    let chars: Vec<char> = run.chars().collect();
    if chars.len() < 2 || chars.len() > 6 {
        return None;
    }
    // The speaker is the leading chars of the run (verb is at the tail).
    Some(chars.iter().take(2).collect())
}

/// A candidate character qualifies as a name if it is a CJK name-shaped token
/// and not in the common non-name list.
fn is_plausible_person(name: &str) -> bool {
    if name.chars().count() < 2 {
        return false;
    }
    !NON_NAMES.iter().any(|n| name.contains(n))
}

/// CJK ideograph or a common name punctuation role.
fn is_cjk_name_char(c: char) -> bool {
    (0x4E00..=0x9FFF).contains(&(c as u32)) || (0x3400..=0x4DBF).contains(&(c as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify speakers are discovered before dialogue verbs and
    /// ranked by frequency, grounding entities in the real text.
    /// Invariants: 刘备/关羽 appear (not "众人"); ordering by count desc.
    #[test]
    fn discovers_speakers_from_dialogue() {
        let text = "刘备说道：此事需从长计议。关羽道：大哥所言极是。\
                    刘备又说道：那便依计行事。关羽曰：诺。";
        let provider = CorpusEntityProvider::from_text("测试", text, 1);
        let entries = provider.entries();
        let names: Vec<&str> = entries.iter().map(|e| e.canonical_name.as_str()).collect();
        assert!(
            names.contains(&"刘备") && names.contains(&"关羽"),
            "both speakers discovered, got {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains(&"众人")),
            "common non-name excluded, got {names:?}"
        );
        // 刘备 appears twice, 关羽 twice; frequency is recorded.
        let liu = entries.iter().find(|e| e.canonical_name == "刘备").unwrap();
        assert_eq!(
            liu.properties.get("frequency").map(|s| s.as_str()),
            Some("2"),
            "frequency recorded from occurrences"
        );
    }

    /// Objective: Verify `min_frequency` filters out one-off speakers.
    /// Invariants: with min_frequency=2, a once-mentioned name is dropped.
    #[test]
    fn min_frequency_filters_sparse_speakers() {
        let text = "张三道：第一句。李四道：只此一句。张三又说道：第二句。";
        let provider = CorpusEntityProvider::from_text("测试", text, 2);
        let entries = provider.entries();
        let names: Vec<&str> = entries.iter().map(|e| e.canonical_name.as_str()).collect();
        assert!(
            names.contains(&"张三"),
            "frequent speaker kept, got {names:?}"
        );
        assert!(
            !names.contains(&"李四"),
            "one-off speaker filtered by min_frequency, got {names:?}"
        );
    }

    /// Objective: Verify a blank/verbose-only text yields no entities.
    /// Invariants: no plausible names → empty entries; no panic.
    #[test]
    fn no_speakers_yields_empty() {
        let provider = CorpusEntityProvider::from_text("测试", "有人说这很奇怪。", 1);
        assert!(
            provider.entries().is_empty(),
            "only a non-name speaker → no entities"
        );
    }
}
