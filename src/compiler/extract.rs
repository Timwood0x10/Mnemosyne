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
use aho_corasick::AhoCorasick;

use crate::compiler::entity::EntityDictionary;
use crate::compiler::{CompileContext, Event, EventParticipant, Mention, Relation};
use crate::entity_resolver::EntityResolver;

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
}

/// Scan sentences for entity mentions, extract events, and populate the context.
///
/// When `resolver` is `Some`, it is used in addition to the dictionary for
/// mention resolution — the resolver handles alias matching and fuzzy
/// embedding lookup, while the dictionary provides the fallback.
pub fn compile(
    ctx: &mut CompileContext,
    sentences: &[&str],
    dict: &EntityDictionary,
    config: &Config,
    resolver: Option<&EntityResolver>,
) {
    let mut current_chapter = ctx.current_timestamp.unwrap_or(1);

    // Build the alias index once for all sentences (ChunkCompiler optimisation)
    let alias_index = AliasIndex::build(dict);

    // Strong verb / action verb patterns → Event(action).
    // Build the Aho-Corasick automaton ONCE per compile call (P3: never
    // rebuild inside the per-sentence loop). Single-pass scan instead of
    // O(N×V) repeated match_indices calls — orders of magnitude faster when
    // V (verb count) × N (sentence count) is large, especially for English.
    let all_verbs: Vec<&str> = config
        .strong_verbs
        .iter()
        .chain(config.action_verbs.iter())
        .map(|s| s.as_str())
        .collect();
    let verb_ac = AhoCorasick::new(&all_verbs).unwrap();

    for text in sentences.iter() {
        if text.len() < 2 {
            continue;
        }

        // Chapter tracking: parse the actual chapter number from "第X回" /
        // "第X章" headings at the START of each iteration, so that events
        // found in the heading sentence and in the body text that follows
        // are both tagged with the correct chapter number. Doing this before
        // the mention scan also ensures heading-only sentences (which may
        // contain no entity mentions) still advance the chapter counter.
        if let Some(ch_num) = parse_chapter_number(text) {
            current_chapter = ch_num;
            ctx.current_timestamp = Some(current_chapter);
        }

        let local_mentions = scan_mentions(text, dict, resolver, Some(&alias_index));
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
                    });
                }
            }
        }

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
                });
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

    // Try Arabic numeral first
    if let Ok(n) = num_str.parse::<i32>() {
        return Some(n);
    }

    // Try Chinese numeral
    chinese_to_int(num_str)
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

/// Scan a single sentence for entity mentions using the dictionary and resolver.
///
/// The dictionary provides the fallback. When `resolver` is `Some`, mentions
/// that the resolver can match (via alias or embedding) are included even if
/// they are not in the dictionary — this is how "刘皇叔" resolves to 刘备
/// without being explicitly listed in the alias map.
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
        let mut aliases: Vec<(String, String)> = dict
            .alias_to_canonical
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        aliases.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
        let patterns: Vec<&str> = aliases.iter().map(|(k, _)| k.as_str()).collect();
        let ac = match patterns.is_empty() {
            true => None,
            false => aho_corasick::AhoCorasick::new(&patterns).ok(),
        };
        AliasIndex { aliases, ac }
    }
}

/// Scan a single sentence for entity mentions using a pre-built alias index.
fn scan_mentions(
    text: &str,
    dict: &EntityDictionary,
    resolver: Option<&EntityResolver>,
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
        // Legacy path — build alias list on every call (fallback)
        let mut aliases: Vec<(String, String)> = dict
            .alias_to_canonical
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        aliases.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
        if !aliases.is_empty() {
            let patterns: Vec<&str> = aliases.iter().map(|(k, _)| k.as_str()).collect();
            if let Ok(ac) = aho_corasick::AhoCorasick::new(&patterns) {
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

    // Resolver-based mention scan: try the resolver for mentions the
    // dictionary didn't already find. This catches aliases like "刘皇叔"
    // that aren't explicitly listed in the alias map.
    if let Some(resolver) = resolver {
        // Walk the text using char indices to avoid UTF-8 slicing issues.
        let char_indices: Vec<(usize, char)> = text.char_indices().collect();
        let mut ci = 0;
        while ci < char_indices.len() {
            let (byte_start, ch) = char_indices[ci];
            // Skip positions already covered by a dictionary mention
            if mentions
                .iter()
                .any(|m| byte_start >= m.offset.start && byte_start < m.offset.end)
            {
                ci += 1;
                continue;
            }
            // Only consider CJK characters as potential mention starts
            if !('\u{4e00}'..='\u{9fff}').contains(&ch) {
                ci += 1;
                continue;
            }
            // Try the resolver on spans of 1-4 additional chars
            let mut matched = false;
            for len in (2..=char_indices.len().saturating_sub(ci).min(6)).rev() {
                let end_idx = ci + len - 1;
                let candidate_byte_end =
                    char_indices[end_idx].0 + char_indices[end_idx].1.len_utf8();
                let candidate = &text[byte_start..candidate_byte_end];
                // Check all chars in candidate are CJK
                if !candidate
                    .chars()
                    .all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
                {
                    continue;
                }
                let result = resolver.resolve(candidate);
                if let Some(entity_id) = result.entity_id() {
                    if !mentions.iter().any(|m| m.offset.start == byte_start) {
                        mentions.push(Mention {
                            sentence_id: 0,
                            entity_id: Some(entity_id),
                            surface: candidate.to_string(),
                            canonical_name: candidate.to_string(),
                            offset: byte_start..candidate_byte_end,
                            confidence: 0.85,
                        });
                    }
                    matched = true;
                    break;
                }
            }
            if !matched {
                ci += 1;
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

    /// Objective: Verify that a simple action sentence creates an Event.
    /// Invariants: At least one event with type "action".
    #[test]
    fn action_sentence_creates_event() {
        let mut ctx = CompileContext::default();
        let dict = make_dict();
        let config = Config::default();
        compile(&mut ctx, &["赵云救阿斗。"], &dict, &config, None);
        assert!(!ctx.events.is_empty(), "should create at least one event");
        let has_action = ctx.events.iter().any(|e| e.event_type == "action");
        assert!(has_action, "should have action-type event");
    }

    /// Objective: Verify that a dialog sentence creates a Dialogue event.
    /// Invariants: Event type is "dialogue"; participants include speaker.
    #[test]
    fn dialog_creates_dialogue_event() {
        let mut ctx = CompileContext::default();
        let dict = make_dict();
        let config = Config::default();
        compile(&mut ctx, &["刘备曰：关羽"], &dict, &config, None);
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
        let refs: Vec<&str> = sentences.to_vec();
        compile(&mut ctx, &refs, &dict, &config, None);
        let has_rel = ctx.relations.iter().any(|r| {
            (r.source == "刘备" && r.target == "关羽") || (r.source == "关羽" && r.target == "刘备")
        });
        assert!(has_rel, "co-occurrence should create a relation");
    }
}
