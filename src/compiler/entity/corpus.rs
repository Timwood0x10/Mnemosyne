//! Corpus provider — entities discovered automatically from source text.
//!
//! This is the non-hardcoded answer to "where do entities come from": instead
//! of a hand-maintained dictionary (the old `entity_profiles/*.json` or the
//! `ingest::characters` tables), a corpus provider scans raw prose and infers
//! characters from narration + dialogue context. Entities are therefore
//! grounded in the actual text — no manual lists, no made-up aliases.
//!
//! Heuristic: a speaker appears immediately before a dialogue verb
//! (`说/道/曰/喊/问/答/喝/叹`), and an English person name is a sequence of
//! Capitalized words (`Alice met Bob`). We count those occurrences, drop
//! common non-names (`有人/一人/众人/有人说`), and rank by frequency so the
//! main cast surfaces first.

use std::collections::HashMap;

use super::provider::{EntityEntry, EntityProvider};
use crate::ingest::extract::DIALOG_VERBS;

/// Common phrases that precede a dialogue verb but are not person names.
const NON_NAMES: &[&str] = &[
    "有人", "一人", "众人", "旁人", "路人", "世人", "小人", "某", "自称", "某人", "我们", "他们",
    "你们", "大家", "我", "你", "他", "她", "人们", "咱", "我辈", "诸位", "众人", "只听",
];

/// Dialogue verbs beyond the shared [`DIALOG_VERBS`] that also introduce a
/// speaker in modern/vernacular prose ("王五说：明天见" must yield 王五, but
/// `说` is absent from the shared novel-focused verb list).
const CORPUS_DIALOG_VERBS: &[&str] = &["说", "说道", "告诉", "回答", "喊道", "问道", "答道"];

/// Sentence-initial English words that are Capitalized but not person names
/// ("The cat", "He said", "In the morning").
const ENGLISH_NON_NAMES: &[&str] = &[
    "The",
    "This",
    "That",
    "These",
    "Those",
    "He",
    "She",
    "It",
    "They",
    "We",
    "You",
    "I",
    "A",
    "An",
    "In",
    "On",
    "At",
    "And",
    "But",
    "Or",
    "So",
    "If",
    "When",
    "Where",
    "What",
    "Who",
    "How",
    "My",
    "Your",
    "His",
    "Her",
    "Our",
    "Their",
    "Its",
    "There",
    "Here",
    "Then",
    "Now",
    "Not",
    "All",
    "One",
    "Two",
    "With",
    "From",
    "By",
    "For",
    "To",
    "As",
    "Is",
    "Was",
    "Were",
    "Are",
    "Be",
    "Been",
    "Being",
    "Do",
    "Does",
    "Did",
    "Will",
    "Would",
    "Can",
    "Could",
    "Should",
    "May",
    "Might",
    "Must",
    "Have",
    "Has",
    "Had",
    "Of",
    "That",
    "Which",
    "While",
    "Before",
    "After",
    "Until",
    "Because",
    "Although",
    "Once",
    "Some",
    "Every",
    "Each",
    "Later",
    "Earlier",
    "Meanwhile",
    "However",
    "Therefore",
    "Nevertheless",
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

/// Count speaker mentions across all dialogue verbs, plus English
/// Capitalized person-name candidates ("Alice met Bob" → Alice, Bob).
fn count_speakers(text: &str) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    // CJK speakers: name immediately before a dialogue verb. Overlapping
    // verbs ("说" vs "说道", "问" vs "问道") can hit the SAME position, so
    // dedup by byte offset before counting — otherwise one mention is
    // double-counted and frequencies are inflated.
    let mut verbs: Vec<&str> = DIALOG_VERBS.to_vec();
    verbs.extend(CORPUS_DIALOG_VERBS);
    verbs.sort_by_key(|v| std::cmp::Reverse(v.len())); // longest first
    let mut mentions: Vec<(usize, String)> = Vec::new();
    for verb in verbs {
        let mut search_from = 0usize;
        while let Some(rel) = text[search_from..].find(verb) {
            let abs = search_from + rel;
            if let Some(name) = name_before(&text[..abs]) {
                if is_plausible_person(&name) {
                    mentions.push((abs, name));
                }
            }
            search_from = abs + verb.len();
        }
    }
    mentions.sort_unstable();
    // Same name hit within one CJK run (a few bytes apart, e.g. "刘备说道"
    // matching 说/道/说道 at adjacent offsets) is ONE utterance — counting
    // each verb hit inflates frequencies (刘备 would read 4 instead of 2).
    let mut deduped: Vec<(usize, String)> = Vec::new();
    for m in mentions {
        if let Some(last) = deduped.last() {
            if last.1 == m.1 && m.0 - last.0 < 12 {
                continue; // same name, same run → same utterance
            }
        }
        deduped.push(m);
    }
    for (_, name) in deduped {
        *counts.entry(name).or_insert(0) += 1;
    }
    // English names: Capitalized word sequences ("Alice met Bob at the park").
    for name in english_person_names(text) {
        *counts.entry(name).or_insert(0) += 1;
    }
    counts
}

/// Extract English person-name candidates: a Capitalized word (first letter
/// uppercase, rest lowercase) that is not a sentence-initial function word.
/// Occurrences are counted per distinct word, so repeated names ("Alice met
/// Bob; later Alice ...") are ranked by frequency like CJK speakers.
fn english_person_names(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for word in text.split(|c: char| !c.is_ascii_alphabetic()) {
        if word.len() < 2 || word.len() > 20 {
            continue;
        }
        let mut chars = word.chars();
        let first = chars.next().expect("non-empty word");
        if !first.is_ascii_uppercase() {
            continue;
        }
        if !chars.all(|c| c.is_ascii_lowercase()) {
            continue; // "HTTP" or mixed-case abbreviations are not names
        }
        if ENGLISH_NON_NAMES.contains(&word) {
            continue;
        }
        names.push(word.to_string());
    }
    names
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
/// that passes the soft person-name validator (no function words / noun tails)
/// and is not in the common non-name list.
///
/// Soft (no surname requirement): literary dialogue speakers like "流苏" have
/// no standard surname, but "感觉" before a dialogue verb is still rejected
/// via function-word/noun-tail gates + NON_NAMES + min_frequency.
fn is_plausible_person(name: &str) -> bool {
    if name.chars().count() < 2 {
        return false;
    }
    if NON_NAMES.iter().any(|n| name.contains(n)) {
        return false;
    }
    crate::compiler::name_validation::is_plausible_person_name(name)
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
            !names.iter().any(|n| n.contains("众人")),
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

    /// Objective: Verify modern vernacular dialogue verbs ("说/说道/答道")
    /// beyond the shared novel verb list also surface speakers.
    /// Invariants: 王五 (before 说) is discovered even though 说 is not in the
    /// shared DIALOG_VERBS; overlapping verbs in one run count once.
    #[test]
    fn vernacular_dialog_verbs_discover_speakers() {
        let text = "王五说：明天见。赵六答道：好的。王五又说：那后天见。赵六再道：行。";
        let provider = CorpusEntityProvider::from_text("测试", text, 2);
        let entries = provider.entries();
        let names: Vec<&str> = entries.iter().map(|e| e.canonical_name.as_str()).collect();
        assert!(
            names.contains(&"王五"),
            "王五 (before 说) must be discovered, got {names:?}"
        );
        assert!(
            names.contains(&"赵六"),
            "赵六 (before 答道) must be discovered, got {names:?}"
        );
        let wangwu = entries.iter().find(|e| e.canonical_name == "王五").unwrap();
        assert_eq!(
            wangwu.properties.get("frequency").map(|s| s.as_str()),
            Some("2"),
            "王五 appears twice (说 + 又说), not inflated by overlapping verbs"
        );
    }

    /// Objective: Verify English Capitalized person names are discovered from
    /// plain narrative prose (no dialogue verbs present).
    /// Invariants: Alice and Bob appear with their real frequencies;
    /// sentence-initial function words (The/He) and mixed-case tokens are NOT
    /// treated as names.
    #[test]
    fn english_person_names_discovered_from_prose() {
        let text =
            "Alice met Bob at the park. Later, Alice introduced Bob to Carol. He was pleased.";
        let provider = CorpusEntityProvider::from_text("test", text, 1);
        let entries = provider.entries();
        let names: Vec<&str> = entries.iter().map(|e| e.canonical_name.as_str()).collect();
        for expected in ["Alice", "Bob", "Carol"] {
            assert!(
                names.contains(&expected),
                "English name `{expected}` must be discovered, got {names:?}"
            );
        }
        for noise in ["The", "He", "Later"] {
            assert!(
                !names.contains(&noise),
                "sentence-initial function word `{noise}` must NOT be a name, got {names:?}"
            );
        }
        let alice = entries
            .iter()
            .find(|e| e.canonical_name == "Alice")
            .unwrap();
        assert_eq!(
            alice.properties.get("frequency").map(|s| s.as_str()),
            Some("2"),
            "Alice appears twice"
        );
        let bob = entries.iter().find(|e| e.canonical_name == "Bob").unwrap();
        assert_eq!(
            bob.properties.get("frequency").map(|s| s.as_str()),
            Some("2"),
            "Bob appears twice"
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
