//! Story Compiler — Pass 2.
//!
//! Extracts Events and Relations from body text using the Entity Registry
//! (built by Pass 1) to resolve mentions to entity IDs.
//!
//! ## Pipeline within Pass 2
//!
//! ```text
//! Sentences → Mention Scan → Observation (SPO) → Event → Relation
//! ```
//!
use crate::compiler::entity::EntityDictionary;
use crate::compiler::{CompileContext, Event, EventParticipant, Mention, Relation};

/// Config for the story compiler's observation extraction.
#[derive(Debug, Clone)]
pub struct Config {
    pub strong_verbs: Vec<String>,
    pub action_verbs: Vec<String>,
    pub dialog_markers: Vec<String>,
    pub proximity_chars: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            // Chinese verbs come from the lexicon registry (single source of
            // truth, corpus-frequency derived). English callers should use
            // `Config::from_language(&EnglishLanguageProvider::new())`.
            strong_verbs: crate::lexicon::global()
                .zh_strong()
                .iter()
                .cloned()
                .collect(),
            action_verbs: crate::lexicon::global()
                .zh_action()
                .iter()
                .cloned()
                .collect(),
            dialog_markers: vec!["曰：".into(), "道：".into(), "言：".into()],
            proximity_chars: 50,
        }
    }
}

impl Config {
    /// Build an extract config from a language provider's verb definitions.
    pub fn from_language(lang: &dyn crate::language::LanguageProvider) -> Self {
        Config {
            strong_verbs: lang.strong_verbs(),
            action_verbs: lang.action_verbs(),
            dialog_markers: Vec::new(),
            proximity_chars: 50,
        }
    }

    /// Config for the language-agnostic production pipeline: the Chinese
    /// defaults (lexicon zh verbs + zh dialog markers) merged with the
    /// English provider's verb tables. `Config::default()` alone is
    /// Chinese-only, so English prose compiled through `compile_source`
    /// yielded zero action events. Dialog markers stay zh-only — English
    /// dialogue detection is not implemented in extract (`from_language`
    /// provides no markers either).
    pub fn bilingual() -> Self {
        let mut cfg = Config::default();
        let en = Config::from_language(&crate::language::EnglishLanguageProvider::new());
        cfg.strong_verbs.extend(en.strong_verbs);
        cfg.action_verbs.extend(en.action_verbs);
        cfg
    }
}

/// Scan sentences for entity mentions, extract events, and populate the context.
///
/// `sentences` is a list of `(text, start_offset, end_offset)` triples —
/// absolute byte spans into the original document — so every Event can carry
/// a re-locatable source span instead of a free-text description prefix.
pub fn compile(
    ctx: &mut CompileContext,
    sentences: &[(&str, usize, usize)],
    dict: &EntityDictionary,
    config: &Config,
) {
    let mut current_chapter = ctx.current_timestamp.unwrap_or(1);

    // Build the alias index once for all sentences (ChunkCompiler optimisation)
    let alias_index = AliasIndex::build(dict);

    // Strong verb / action verb patterns → Event(action).
    // Build the Aho-Corasick automaton ONCE per compile call (P3: never
    // rebuild inside the per-sentence loop). Single-pass scan instead of
    // O(N×V) repeated match_indices calls — orders of magnitude faster when
    // V (verb count) × N (sentence count) is large, especially for English.
    // Filter empty patterns first: AhoCorasick::new fails on empty strings
    // (reachable via user-editable LanguageProvider verb lists), not only on
    // an empty set. After filtering, degrade to no-verb-scan instead of
    // panicking.
    let all_verbs: Vec<&str> = config
        .strong_verbs
        .iter()
        .chain(config.action_verbs.iter())
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .collect();
    let verb_ac = if all_verbs.is_empty() {
        None
    } else {
        // `MatchKind::LeftmostLongest`, NOT the default `Standard`: Standard
        // reports the match that ENDS earliest, so a shorter verb shadows the
        // longer extension of it (`杀` swallows `杀害`) — the wrong verb, the
        // wrong span and the wrong FactType. LeftmostLongest resolves the
        // longest form at a position instead. Mirrors `lexicon::matcher`.
        match aho_corasick::AhoCorasickBuilder::new()
            .match_kind(aho_corasick::MatchKind::LeftmostLongest)
            .build(&all_verbs)
        {
            Ok(ac) => Some(ac),
            Err(e) => {
                tracing::warn!(error = %e, "verb automaton build failed; skipping verb scan");
                None
            }
        }
    };

    for &(text, sent_start, sent_end) in sentences {
        if text.len() < 2 {
            continue;
        }

        // Timeline tracking: in-book years (e.g. "In 1805") are the PRIMARY
        // timeline for novels without chapter headings (English texts);
        // "第X回/第X章" chapter numbers are the fallback (Chinese novels).
        // Both carry forward: an event keeps the last seen timeline marker
        // until the next one appears. Year detection runs first so it wins
        // when both kinds of markers exist.
        if let Some(year) = parse_in_book_year(text) {
            current_chapter = year;
            ctx.current_timestamp = Some(current_chapter);
        } else if let Some(ch_num) = parse_chapter_number(text) {
            current_chapter = ch_num;
            ctx.current_timestamp = Some(current_chapter);
        }

        let local_mentions = scan_mentions(text, dict, Some(&alias_index));
        if local_mentions.is_empty() {
            continue;
        }

        // Dialog pattern: X曰/Y道 → Event(dialogue)
        for marker in &config.dialog_markers {
            if let Some(pos) = text.find(marker.as_str()) {
                let speaker = local_mentions.iter().rfind(|m| m.offset.end <= pos);
                let addressee = local_mentions
                    .iter()
                    .find(|m| m.offset.start >= pos + marker.len());

                if let Some(s) = speaker {
                    let mut participants = vec![EventParticipant {
                        entity_name: s.canonical_name.clone(),
                        role: "speaker".into(),
                    }];
                    if let Some(a) = addressee {
                        participants.push(EventParticipant {
                            entity_name: a.canonical_name.clone(),
                            role: "addressee".into(),
                        });
                    }
                    ctx.events.push(Event {
                        effects: vec![],
                        id: None,
                        title: format!("{}曰", s.canonical_name),
                        event_type: "dialogue".into(),
                        timestamp: Some(current_chapter),
                        location: None,
                        description: text[..pos.min(text.len())].to_string(),
                        participants,
                        importance: 0.5,
                        // Absolute span of the source sentence — dialogue
                        // markers are located inside `text`, but the claim is
                        // the whole sentence (evidence-traceable).
                        start_offset: Some(sent_start),
                        end_offset: Some(sent_end),
                    });
                }
            }
        }

        if let Some(verb_ac) = &verb_ac {
            for m in verb_ac.find_iter(text) {
                let verb = &all_verbs[m.pattern()];
                let pos = m.start();
                let subject = local_mentions.iter().rfind(|mention| {
                    mention.offset.end <= pos && (pos - mention.offset.end) < config.proximity_chars
                });

                let object = local_mentions.iter().find(|mention| {
                    mention.offset.start >= pos + verb.len()
                        && (mention.offset.start - (pos + verb.len())) < config.proximity_chars
                });

                if let Some(s) = subject {
                    let mut title = format!("{} {}", s.canonical_name, verb);
                    let mut participants = vec![EventParticipant {
                        entity_name: s.canonical_name.clone(),
                        role: "subject".into(),
                    }];
                    if let Some(o) = object {
                        title = format!("{} {} {}", s.canonical_name, verb, o.canonical_name);
                        participants.push(EventParticipant {
                            entity_name: o.canonical_name.clone(),
                            role: "object".into(),
                        });
                    }

                    ctx.events.push(Event {
                        effects: vec![],
                        id: None,
                        title,
                        event_type: "action".into(),
                        timestamp: Some(current_chapter),
                        location: None,
                        description: text[..pos.min(text.len())].to_string(),
                        participants,
                        importance: 0.6,
                        // Verb match span within the sentence, promoted to
                        // absolute document offsets for evidence tracing.
                        start_offset: Some(sent_start + pos),
                        end_offset: Some((sent_start + pos + verb.len()).min(sent_end)),
                    });
                }
            }
        }
    }
    // Build relations from co-occurring event participants
    build_relations(ctx);
}

/// Parse a chapter number from a heading like "第三回" or "第120章".
///
/// Supports both Arabic numerals ("第1回") and Chinese numerals
/// ("第一百二十回"). Returns `None` if no chapter heading is found.
///
/// Private: chapter tracking is driven entirely from [`compile`], which calls
/// this on each sentence.
fn parse_chapter_number(text: &str) -> Option<i32> {
    // Only treat the sentence as a chapter heading when "第" appears at the
    // START (after trimming leading whitespace). This prevents false-positive
    // chapter resets on narrative text that merely contains "第X回" somewhere
    // in the middle, e.g. a character saying "第三回 合该如此".
    let trimmed = text.trim_start();
    if !trimmed.starts_with("第") {
        return None;
    }
    let after = &trimmed["第".len()..];
    // Find the end marker (回 or 章)
    let end = after.find("回").or_else(|| after.find("章"))?;
    let num_str = &after[..end];

    // All-numerals validation (mirrors ingest::corpus::is_valid_chapter_numeral):
    // "第三回合，二马相交" must NOT reset the timeline — the 回 of 回合 is not
    // a chapter delimiter. Reject non-numeral content and non-positive values.
    const CN_NUMERALS: &[char] = &[
        '零', '一', '二', '三', '四', '五', '六', '七', '八', '九', '十', '百', '千', '万',
    ];
    if num_str.is_empty()
        || !num_str
            .chars()
            .all(|c| c.is_ascii_digit() || CN_NUMERALS.contains(&c))
    {
        return None;
    }

    // Try Arabic numeral first
    if let Ok(n) = num_str.parse::<i32>() {
        return (n > 0).then_some(n);
    }

    // Try Chinese numeral
    let n = chinese_to_int(num_str)?;
    (n > 0).then_some(n)
}

/// Parse an in-book year marker (e.g. "In 1805", "1807,") as the PRIMARY
/// timeline for novels without chapter headings (English texts like War and
/// Peace). Returns `None` unless a standalone 4-digit year in a plausible
/// range (1700–2100) is found; the year must NOT be embedded in a longer
/// number (e.g. a 5+ digit id) to avoid false positives.
///
/// Mid-sentence standalone years remain valid (`"the winter of 1812."`) —
/// that is the locked contract of `in_book_year_detection`. The carry-forward
/// risk from body digits is mitigated by requiring a *standalone* 4-digit
/// run (same as before), not by restricting to sentence-initial position.
fn parse_in_book_year(text: &str) -> Option<i32> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if bytes[i].is_ascii_digit() {
            // A 4-digit run bounded by non-digits (or string edges).
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let run = &text[start..i];
            if run.len() == 4 {
                let year: i32 = run.parse().ok()?;
                if (1700..=2100).contains(&year) {
                    return Some(year);
                }
            }
            continue;
        }
        i += 1;
    }
    None
}

/// Convert a Chinese numeral string (e.g. "一百二十") to an integer.
fn chinese_to_int(s: &str) -> Option<i32> {
    const DIGITS: &[(&str, i32)] = &[
        ("零", 0),
        ("〇", 0),
        ("一", 1),
        ("二", 2),
        ("两", 2),
        ("三", 3),
        ("四", 4),
        ("五", 5),
        ("六", 6),
        ("七", 7),
        ("八", 8),
        ("九", 9),
    ];
    const UNITS: &[(&str, i32)] = &[("十", 10), ("百", 100), ("千", 1000)];

    let mut total: i32 = 0;
    let mut current: i32 = 0;

    for ch in s.chars() {
        let cs = ch.to_string();
        // Check if it's a digit
        if let Some((_, val)) = DIGITS.iter().find(|(k, _)| *k == cs) {
            current = *val;
        } else if let Some((_, unit)) = UNITS.iter().find(|(k, _)| *k == cs) {
            if current == 0 {
                current = 1; // "十" alone means 10
            }
            total += current * unit;
            current = 0;
        } else {
            return None; // unknown character
        }
    }
    total += current;
    if total > 0 { Some(total) } else { None }
}

/// Scan a single sentence for entity mentions using the dictionary.
///
/// Pre-built entity alias index used by all sentences in a compile run.
///
/// Building the Aho-Corasick automaton once per compile (instead of once
/// per sentence, which is the current `scan_mentions` behaviour) reduces
/// the bottleneck from O(S×A) to O(S+A) where S is sentence count and A
/// is alias count — critical for English novels with ~17k sentences.
struct AliasIndex {
    // (alias, canonical_name) pairs in longest-first order
    aliases: Vec<(String, String)>,
    // Optional Aho-Corasick automaton (None if empty)
    ac: Option<aho_corasick::AhoCorasick>,
}

impl AliasIndex {
    fn build(dict: &EntityDictionary) -> Self {
        // Single-character aliases (`single_char`: 云/飞/操) are excluded
        // from the global Aho-Corasick automaton: a whole-text scan has no
        // safety context, so "浮云" would wrongly resolve to 赵云. Single
        // chars must only match via the context-checked shortname path
        // (`ingest::extract::find_single_char_matches`), which requires
        // punctuation-before + verb-after.
        let mut aliases: Vec<(String, String)> = dict
            .alias_to_canonical
            .iter()
            .filter(|(k, _)| k.chars().count() >= 2)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        aliases.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
        let patterns: Vec<&str> = aliases.iter().map(|(k, _)| k.as_str()).collect();
        let ac = match patterns.is_empty() {
            true => None,
            false => {
                // `LeftmostLongest` makes the long alias win over a prefix of
                // it (`诸葛亮` over `诸葛`) — the reason the reverse-length
                // sort above exists. Under the default `Standard` the shorter
                // prefix ends earliest and resolves the WRONG entity.
                match aho_corasick::AhoCorasickBuilder::new()
                    .match_kind(aho_corasick::MatchKind::LeftmostLongest)
                    .build(&patterns)
                {
                    Ok(ac) => Some(ac),
                    Err(e) => {
                        // Empty-string aliases are filtered above, but a
                        // pathological dict could still fail the builder —
                        // degrade to the legacy per-call path rather than
                        // silently dropping all dictionary mentions with .ok().
                        tracing::warn!(error = %e, "alias automaton build failed; using legacy scan");
                        None
                    }
                }
            }
        };
        AliasIndex { aliases, ac }
    }
}

/// Scan a single sentence for entity mentions using a pre-built alias index.
fn scan_mentions(
    text: &str,
    dict: &EntityDictionary,
    alias_index: Option<&AliasIndex>,
) -> Vec<Mention> {
    let mut mentions = Vec::new();

    if let Some(idx) = alias_index {
        // Use pre-built automaton (built once per compile)
        if let Some(ref ac) = idx.ac {
            for m in ac.find_iter(text) {
                let alias = &idx.aliases[m.pattern()].0;
                let pos = m.start();
                if mentions.iter().any(|existing: &Mention| {
                    pos >= existing.offset.start && pos < existing.offset.end
                }) {
                    continue;
                }
                let canonical = idx.aliases[m.pattern()].1.clone();
                let (_, entity_id) = dict.resolve(alias).unwrap_or((canonical.clone(), None));
                mentions.push(Mention {
                    sentence_id: 0,
                    entity_id,
                    surface: alias.to_string(),
                    canonical_name: canonical,
                    offset: pos..(pos + alias.len()),
                    confidence: 0.9,
                });
            }
        }
        // Resolver fallback runs regardless of alias_index
    } else {
        // Legacy path — build alias list on every call (fallback).
        // Same single-char exclusion as AliasIndex::build: a bare "云" must
        // never match via whole-text scan (浮云 → 赵云 false positive).
        let mut aliases: Vec<(String, String)> = dict
            .alias_to_canonical
            .iter()
            .filter(|(k, _)| k.chars().count() >= 2)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        aliases.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
        if !aliases.is_empty() {
            let patterns: Vec<&str> = aliases.iter().map(|(k, _)| k.as_str()).collect();
            // `LeftmostLongest` for the same reason as the indexed path: the
            // longest alias must win over a prefix of it. Failure keeps the
            // legacy behaviour of skipping the scan (`if let Ok`).
            if let Ok(ac) = aho_corasick::AhoCorasickBuilder::new()
                .match_kind(aho_corasick::MatchKind::LeftmostLongest)
                .build(&patterns)
            {
                for m in ac.find_iter(text) {
                    let alias = &patterns[m.pattern()];
                    let pos = m.start();
                    if mentions.iter().any(|existing: &Mention| {
                        pos >= existing.offset.start && pos < existing.offset.end
                    }) {
                        continue;
                    }
                    let canonical = aliases[m.pattern()].1.clone();
                    let (_, entity_id) = dict.resolve(alias).unwrap_or((canonical.clone(), None));
                    mentions.push(Mention {
                        sentence_id: 0,
                        entity_id,
                        surface: alias.to_string(),
                        canonical_name: canonical,
                        offset: pos..(pos + alias.len()),
                        confidence: 0.9,
                    });
                }
            }
        }
    }

    mentions.sort_by_key(|a| a.offset.start);
    mentions.dedup_by(|a, b| a.offset.start == b.offset.start);
    mentions
}

/// Build relations from events — entities that appear in the same event
/// multiple times with consistent roles become a relation.
fn build_relations(ctx: &mut CompileContext) {
    for event in &ctx.events {
        for pair in event_pairs(&event.participants) {
            let already = ctx.relations.iter().any(|r| {
                (r.source == pair.0 && r.target == pair.1)
                    || (r.source == pair.1 && r.target == pair.0)
            });
            if !already {
                ctx.relations.push(Relation {
                    source: pair.0.clone(),
                    target: pair.1.clone(),
                    relation_type: "associated".into(),
                    valid_from: event.timestamp,
                    valid_to: None,
                    confidence: 0.5,
                });
            }
        }
    }
}

/// Generate all unique pairs from a participant list.
fn event_pairs(participants: &[EventParticipant]) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for i in 0..participants.len() {
        for j in (i + 1)..participants.len() {
            let a = &participants[i].entity_name;
            let b = &participants[j].entity_name;
            if a != b {
                pairs.push((a.clone(), b.clone()));
            }
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::entity::EntityDictionary;

    /// Objective: Verify in-book year markers are detected as the primary
    /// timeline (War-and-Peace style English texts without chapter headings).
    /// Invariants: "In 1805", "1807," and "…1812." yield the expected years;
    /// a year embedded in a longer number (id) is NOT matched; out-of-range
    /// 4-digit numbers and non-year text yield None.
    #[test]
    fn in_book_year_detection() {
        assert_eq!(
            parse_in_book_year("In 1805 Prince was silent."),
            Some(1805),
            "an in-book year after In must be parsed"
        );
        assert_eq!(parse_in_book_year("1807,"), Some(1807));
        assert_eq!(
            parse_in_book_year("the winter of 1812."),
            Some(1812),
            "a year after the winter of must be parsed"
        );
        assert_eq!(
            parse_in_book_year("sentence 1805 continues"),
            Some(1805),
            "a year between words must still be parsed"
        );

        // Embedded in a longer digit run → not a standalone year.
        assert_eq!(
            parse_in_book_year("id=18056"),
            None,
            "5-digit run is not a year"
        );
        assert_eq!(
            parse_in_book_year("id=118057"),
            None,
            "embedded 5-digit run rejected"
        );

        // Out of the plausible historical range.
        assert_eq!(parse_in_book_year("year 0999"), None, "999 out of range");
        assert_eq!(parse_in_book_year("year 9999"), None, "9999 out of range");

        // No digits at all.
        assert_eq!(
            parse_in_book_year("no year here"),
            None,
            "text without a year must yield None"
        );
        assert_eq!(
            parse_in_book_year(""),
            None,
            "an empty string must yield None"
        );
    }

    /// Objective: Verify chapter headings still parse (fallback timeline).
    /// Invariants: Chinese "第X回/第X章" headings map to numbers; narrative
    /// text with "第X回" mid-sentence is rejected (start-anchored rule).
    #[test]
    fn chapter_number_fallback_still_works() {
        assert_eq!(
            parse_chapter_number("第三回 桃园结义"),
            Some(3),
            "Chinese numerals in 第X回 must be parsed"
        );
        assert_eq!(
            parse_chapter_number("第120章"),
            Some(120),
            "Arabic numerals in 第X章 must be parsed"
        );
        assert_eq!(
            parse_chapter_number("他说：第三回合该如此"),
            None,
            "a 回 inside prose must not be a chapter number"
        );
        assert_eq!(
            parse_chapter_number("plain text"),
            None,
            "text without a chapter marker must yield None"
        );
    }

    fn make_dict() -> EntityDictionary {
        let mut d = EntityDictionary::default();
        d.alias_to_canonical.insert("刘备".into(), "刘备".into());
        d.alias_to_canonical.insert("关羽".into(), "关羽".into());
        d.alias_to_canonical.insert("张飞".into(), "张飞".into());
        d.alias_to_canonical.insert("赵云".into(), "赵云".into());
        d.alias_to_canonical.insert("阿斗".into(), "阿斗".into());
        d.alias_to_canonical.insert("曹操".into(), "曹操".into());
        d
    }

    /// Objective: Verify single-char aliases (云/飞/操) are excluded from the
    /// whole-text Aho-Corasick automaton so mid-word occurrences ("浮云") do
    /// NOT resolve to a character (赵云).
    /// Invariants: a dict carrying "云"→"赵云" matches "浮云" nowhere via the
    /// global scan, while the multi-char alias "赵云" still matches.
    #[test]
    fn single_char_aliases_excluded_from_automaton() {
        let mut dict = make_dict();
        dict.alias_to_canonical.insert("云".into(), "赵云".into());
        let idx = AliasIndex::build(&dict);
        let ac = idx.ac.expect("automaton built with multi-char aliases");
        assert!(
            ac.find_iter("浮云蔽日").next().is_none(),
            "bare 云 must not match inside 浮云"
        );
        assert!(
            ac.find_iter("赵云救阿斗").next().is_some(),
            "multi-char 赵云 still matches"
        );
        // The excluded single-char alias must not even be a pattern.
        assert!(
            idx.aliases.iter().all(|(k, _)| k.chars().count() >= 2),
            "single-char aliases are filtered out of the index"
        );
    }

    /// Objective: Verify the legacy (non-index) scan path applies the same
    /// single-char exclusion.
    /// Invariants: scan_mentions without an AliasIndex does not emit a 赵云
    /// mention for "浮云蔽日", but still finds 曹操 in "曹操观云".
    #[test]
    fn legacy_path_excludes_single_char() {
        let dict = make_dict();
        let mentions = scan_mentions("浮云蔽日", &dict, None);
        assert!(
            mentions.is_empty(),
            "bare 云 must not match in the legacy path, got {mentions:?}"
        );
        let mentions = scan_mentions("曹操观云", &dict, None);
        assert!(
            mentions.iter().any(|m| m.canonical_name == "曹操"),
            "multi-char alias still found in legacy path"
        );
    }

    /// Objective: Verify that a simple action sentence creates an Event.
    /// Invariants: At least one event with type "action".
    #[test]
    fn action_sentence_creates_event() {
        let mut ctx = CompileContext::default();
        let dict = make_dict();
        let config = Config::default();
        compile(&mut ctx, &[("赵云救阿斗。", 0, 12)], &dict, &config);
        assert!(!ctx.events.is_empty(), "should create at least one event");
        let has_action = ctx.events.iter().any(|e| e.event_type == "action");
        assert!(has_action, "should have action-type event");
    }

    /// Objective: Verify `Config::bilingual()` covers BOTH languages — the
    /// zh-only default yields zero action events for English prose (the
    /// pipeline regression), while the bilingual config extracts them.
    /// Invariants: default → no action events on English text; bilingual →
    /// an action event whose span is non-empty; zh dialog markers survive.
    #[test]
    fn bilingual_config_extracts_english_actions() {
        let mut dict = EntityDictionary::default();
        dict.register_discovered("Prince John", &[]);
        let text = "Prince John killed Count Dracula in the hallway.";
        let sentences = [(text, 0usize, text.len())];

        let mut zh_ctx = CompileContext::default();
        compile(&mut zh_ctx, &sentences, &dict, &Config::default());
        assert!(
            !zh_ctx.events.iter().any(|e| e.event_type == "action"),
            "zh-only default must not match English verbs, got {:?}",
            zh_ctx.events
        );

        let mut bi_ctx = CompileContext::default();
        compile(&mut bi_ctx, &sentences, &dict, &Config::bilingual());
        let ev = bi_ctx
            .events
            .iter()
            .find(|e| e.event_type == "action")
            .unwrap_or_else(|| panic!("bilingual config must yield an action event"));
        assert!(
            ev.title.contains("Prince John"),
            "subject resolved from the dict, got title {}",
            ev.title
        );
        let (Some(s), Some(e)) = (ev.start_offset, ev.end_offset) else {
            panic!("action event must carry a verb span");
        };
        assert!(s < e, "verb span must be non-empty, got {s}..{e}");

        // Dialog markers stay Chinese-only (extract has no en markers by
        // design) — merging must not drop the zh defaults.
        let cfg = Config::bilingual();
        assert!(
            cfg.dialog_markers.contains(&"曰：".to_string()),
            "zh dialog markers must survive the merge"
        );
        assert!(
            cfg.strong_verbs.iter().any(|v| v == "kill"),
            "en strong verbs must be merged"
        );
    }

    /// Objective: Verify that a dialog sentence creates a Dialogue event.
    /// Invariants: Event type is "dialogue"; participants include speaker.
    #[test]
    fn dialog_creates_dialogue_event() {
        let mut ctx = CompileContext::default();
        let dict = make_dict();
        let config = Config::default();
        compile(&mut ctx, &[("刘备曰：关羽", 0, 15)], &dict, &config);
        let has_dialogue = ctx.events.iter().any(|e| e.event_type == "dialogue");
        assert!(has_dialogue, "dialog sentence should create dialogue event");
    }

    /// Objective: Verify that repeated co-occurrence creates a relation.
    /// Invariants: A relation exists between 刘备 and 关羽.
    #[test]
    fn co_occurrence_creates_relation() {
        let mut ctx = CompileContext::default();
        let dict = make_dict();
        let config = Config::default();
        let sentences = ["刘备救关羽。", "刘备救张飞。"];
        let refs: Vec<(&str, usize, usize)> =
            sentences.iter().map(|s| (*s, 0usize, s.len())).collect();
        compile(&mut ctx, &refs, &dict, &config);
        let has_rel = ctx.relations.iter().any(|r| {
            (r.source == "刘备" && r.target == "关羽") || (r.source == "关羽" && r.target == "刘备")
        });
        assert!(has_rel, "co-occurrence should create a relation");
    }

    /// Objective: Verify the alias automaton reports the LONGER alias when one
    /// alias is a prefix of another (`诸葛` vs `诸葛亮`) — the production
    /// failure under `MatchKind::Standard`.
    /// Invariants: scanning `诸葛亮` yields the 3-char pattern, never its prefix.
    #[test]
    fn alias_automaton_prefers_longer_prefix() {
        let mut dict = EntityDictionary::default();
        dict.alias_to_canonical.insert("诸葛".into(), "诸葛".into());
        dict.alias_to_canonical
            .insert("诸葛亮".into(), "诸葛亮".into());
        let idx = AliasIndex::build(&dict);
        let ac = idx.ac.as_ref().expect("alias automaton must build");
        let m = ac.find_iter("诸葛亮").next().expect("a match must exist");
        assert_eq!(
            idx.aliases[m.pattern()].0,
            "诸葛亮",
            "the longest alias must win over its prefix"
        );
    }

    /// Objective: Verify the production verb automaton reports the LONGER verb
    /// form (`杀害`) when the shorter `杀` is also a pattern.
    /// Invariants: the action event title carries `杀害` and its recorded span
    /// length equals the 2-char verb, not the 1-char prefix.
    #[test]
    fn verb_automaton_prefers_longer_form() {
        let dict = make_dict();
        let config = Config {
            strong_verbs: vec!["杀".into(), "杀害".into()],
            action_verbs: vec![],
            dialog_markers: vec![],
            proximity_chars: 50,
        };
        let mut ctx = CompileContext::default();
        let text = "刘备杀害了他";
        compile(&mut ctx, &[(text, 0, text.len())], &dict, &config);
        let ev = ctx
            .events
            .iter()
            .find(|e| e.event_type == "action")
            .expect("an action event must be produced");
        assert!(
            ev.title.contains("杀害"),
            "the longer verb must win, got title {}",
            ev.title
        );
        let (Some(s), Some(e)) = (ev.start_offset, ev.end_offset) else {
            panic!("action event must carry a verb span");
        };
        assert_eq!(e - s, "杀害".len(), "span must cover the full longer verb");
    }

    /// Objective: Verify the shorter verb still matches when the longer form is
    /// absent from the text/pattern set.
    /// Invariants: `杀` alone still yields an action event whose span equals the
    /// 1-char verb.
    #[test]
    fn shorter_verb_matches_when_longer_absent() {
        let dict = make_dict();
        let config = Config {
            strong_verbs: vec!["杀".into(), "杀害".into()],
            action_verbs: vec![],
            dialog_markers: vec![],
            proximity_chars: 50,
        };
        let mut ctx = CompileContext::default();
        let text = "刘备杀了他";
        compile(&mut ctx, &[(text, 0, text.len())], &dict, &config);
        let ev = ctx
            .events
            .iter()
            .find(|e| e.event_type == "action")
            .expect("an action event must be produced");
        assert!(
            ev.title.contains("杀") && !ev.title.contains("杀害"),
            "the shorter verb must match when the longer is absent, got {}",
            ev.title
        );
        let (Some(s), Some(e)) = (ev.start_offset, ev.end_offset) else {
            panic!("action event must carry a verb span");
        };
        assert_eq!(e - s, "杀".len(), "span must cover the shorter verb");
    }
}
