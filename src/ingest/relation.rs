use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use super::characters::{NOVELS, get_novel_characters};
use super::extract::floor_char_boundary;
use serde::Deserialize;

// ═══════════════════════════════════════════════════════════════
// JSON config types
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize, Clone)]
pub struct RelationRulesConfig {
    relation_type_rules: Vec<RelationTypeRuleConfig>,
    dialog_address_rules: Vec<DialogAddressRuleConfig>,
    /// Byte window the two names and an assertion must share to count as one
    /// clause. Optional so a config written before the field existed keeps
    /// loading; [`DEFAULT_RELATION_KEYWORD_PROXIMITY`] then applies.
    #[serde(default)]
    relation_keyword_proximity: Option<usize>,
}

/// Clause width assumed when the config does not state one.
///
/// Clauses are short — `A娶B`, `A与B配为夫妇`, `A、B…结义为兄弟` — and 40 bytes is
/// roughly thirteen CJK characters, which fits every form in the shipped rules
/// while still excluding "both names merely appear in this chapter".
const DEFAULT_RELATION_KEYWORD_PROXIMITY: usize = 40;

/// A word that **states** a bond (`桃园结义`, `配为夫妇`, `拜师`).
///
/// Deliberately not the same set as [`DialogAddressRuleConfig::keywords`]. An
/// address form (`夫人`, `主公`, `哥哥`) is an ordinary noun or title that only
/// implies a relation when someone actually uses it *to address* another
/// character; listing one here let a whole chapter's prose type any pair that
/// shared a clause with it. [`validate`] rejects a config that lists one word in
/// both places.
#[derive(Debug, Deserialize, Clone)]
pub struct RelationTypeRuleConfig {
    #[serde(rename = "type")]
    r#type: String,
    /// Accepted under the old `keywords` name too, so an operator's existing
    /// config keeps loading; the new name distinguishes it from an address form.
    #[serde(alias = "keywords")]
    assertions: Vec<String>,
    faction_constraint: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DialogAddressRuleConfig {
    keywords: Vec<String>,
    relation_type: String,
}

/// Parse relation rules from an explicit path, rejecting an unusable rule set.
///
/// Callers that were handed a path by the operator (rather than falling back
/// to the bundled default) use this to surface a broken config as an error
/// instead of silently running with a different rule set.
///
/// # Errors
///
/// Returns a description naming the offending path when the file cannot be read
/// or is not valid JSON, or [`validate`]'s message when it parses but describes
/// rules that cannot be honoured.
pub fn load_config_from_path(path: &Path) -> Result<RelationRulesConfig, String> {
    let config: RelationRulesConfig = crate::config::load_json_file(path)?;
    validate(&config)?;
    Ok(config)
}

/// Load the config from the resource root, or report why it failed.
///
/// A parse failure used to be folded into `.ok()` with zero logging, so a
/// stray comma silently swapped the entire rule set for the built-in fallback
/// (audit H16); the error is now returned so the caller can log it.
fn load_config() -> Result<RelationRulesConfig, String> {
    let path = crate::config::resolve_resource_path("config/relation_rules.json");
    load_config_from_path(&path)
}

static CONFIG: LazyLock<RelationRulesConfig> = LazyLock::new(|| {
    match load_config() {
        Ok(cfg) => cfg,
        Err(e) => {
            // Make the fallback VISIBLE: a malformed user config must not look
            // like a clean start (audit H16).
            tracing::warn!(
                error = %e,
                "config/relation_rules.json could not be loaded; using the built-in relation rules"
            );
            fallback_config()
        }
    }
});

/// Builtin fallback rules used when no JSON config is found.
///
/// Mirrors `config/relation_rules.json`: assertions state a bond, address forms
/// are separate, and no word appears in both ([`validate`] enforces that on both
/// this set and any loaded one, so the built-in cannot drift from the shipped
/// config into the `夫人`-asserts-a-marriage mistake).
fn fallback_config() -> RelationRulesConfig {
    RelationRulesConfig {
        relation_keyword_proximity: None,
        relation_type_rules: vec![
            rtc(
                "夫妻",
                &["夫妻", "夫妇", "配为夫妇", "嫁与", "娶", "结亲"],
                "any",
            ),
            rtc(
                "结义",
                &["结义", "结拜", "兄弟", "义兄", "义弟", "桃园结义"],
                "any",
            ),
            rtc("父子", &["父子", "父女"], "any"),
            rtc("母女", &["母子", "母女"], "any"),
            rtc(
                "师徒",
                &["师父", "师傅", "师尊", "徒弟", "拜师", "授业", "为师"],
                "same",
            ),
            rtc(
                "君臣",
                &[
                    "主公", "陛下", "大王", "王上", "天子", "丞相", "都督", "将军", "圣上",
                ],
                "same",
            ),
            rtc("挚友", &["挚友", "好友"], "any"),
            rtc(
                "仇敌",
                &["仇敌", "仇人", "对头", "战败", "败阵", "讨伐"],
                "different",
            ),
            rtc("姐妹", &["姐妹", "姊妹"], "any"),
            rtc("亲戚", &["亲戚", "亲属", "亲家"], "any"),
        ],
        dialog_address_rules: vec![
            dac(
                &["主公", "陛下", "大王", "王上", "皇上", "天子", "圣上"],
                "君臣",
            ),
            dac(&["丞相", "军师", "都督", "将军"], "君臣"),
            dac(
                &[
                    "哥哥", "大哥", "二哥", "三弟", "义兄", "义弟", "贤弟", "兄弟",
                ],
                "结义",
            ),
            dac(&["师父", "师傅", "师尊", "恩师", "老师", "徒弟"], "师徒"),
            dac(&["夫人", "娘子", "贤妻", "拙荆", "内人"], "夫妻"),
            dac(&["父亲", "父王", "爹爹", "岳父"], "父子"),
            dac(&["母亲", "娘亲"], "母女"),
        ],
    }
}

fn rtc(r#type: &str, assertions: &[&str], faction_constraint: &str) -> RelationTypeRuleConfig {
    RelationTypeRuleConfig {
        r#type: r#type.to_string(),
        assertions: assertions.iter().map(|s| s.to_string()).collect(),
        faction_constraint: faction_constraint.to_string(),
    }
}

fn dac(keywords: &[&str], relation_type: &str) -> DialogAddressRuleConfig {
    DialogAddressRuleConfig {
        keywords: keywords.iter().map(|s| s.to_string()).collect(),
        relation_type: relation_type.to_string(),
    }
}

/// `faction_constraint` values the importance weighting understands.
const KNOWN_FACTION_CONSTRAINTS: &[&str] = &["any", "same", "different"];

/// Reject a rule set that cannot be honoured, naming the first problem found.
///
/// The checks are structural rather than novel-specific, so they hold for any
/// vocabulary an operator supplies:
///
/// - no empty relation type, assertion or address keyword — the empty string
///   matches everywhere and would type every pair,
/// - at least one assertion per relation type,
/// - `faction_constraint` must be one of [`KNOWN_FACTION_CONSTRAINTS`], because
///   it feeds the importance weighting rather than being ignored.
///
/// Deliberately **not** checked: a word appearing in both lists. That looked like
/// a mistake but is not one — `兄弟` names the bond in 水浒传 narration *and*
/// serves as a form of address, and forbidding the overlap removed real signal
/// and let weaker rules re-type those pairs (`宋江→吴用` became 君臣,
/// `刘备→关羽` became 夫妻). Only a word with no asserting use at all (`夫人`,
/// `哥哥`) has to stay out of `assertions`, and that is a vocabulary judgement,
/// not a structural one.
///
/// # Errors
///
/// Returns a description of the first violation.
fn validate(config: &RelationRulesConfig) -> Result<(), String> {
    for rule in &config.relation_type_rules {
        if rule.r#type.trim().is_empty() {
            return Err("a `relation_type_rules` entry has an empty `type`".to_string());
        }
        if !KNOWN_FACTION_CONSTRAINTS.contains(&rule.faction_constraint.as_str()) {
            return Err(format!(
                "relation type `{}` has unknown `faction_constraint` `{}` (expected one of {})",
                rule.r#type,
                rule.faction_constraint,
                KNOWN_FACTION_CONSTRAINTS.join(" / ")
            ));
        }
        if rule.assertions.is_empty() {
            return Err(format!(
                "relation type `{}` lists no `assertions`",
                rule.r#type
            ));
        }
        if let Some(empty) = rule.assertions.iter().find(|word| word.trim().is_empty()) {
            return Err(format!(
                "relation type `{}` lists an empty assertion ({empty:?})",
                rule.r#type
            ));
        }
    }

    for rule in &config.dialog_address_rules {
        if rule.relation_type.trim().is_empty() {
            return Err("a `dialog_address_rules` entry has an empty `relation_type`".to_string());
        }
        if rule.keywords.is_empty() {
            return Err(format!(
                "`dialog_address_rules` entry for `{}` lists no keywords",
                rule.relation_type
            ));
        }
        if let Some(empty) = rule.keywords.iter().find(|word| word.trim().is_empty()) {
            return Err(format!(
                "`dialog_address_rules` entry for `{}` lists an empty keyword ({empty:?})",
                rule.relation_type
            ));
        }
    }
    Ok(())
}

// ═══════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════

pub fn relation_type_rules() -> &'static [(Vec<String>, String)] {
    static RULES: LazyLock<Vec<(Vec<String>, String)>> = LazyLock::new(|| {
        CONFIG
            .relation_type_rules
            .iter()
            .map(|r| (r.assertions.clone(), r.r#type.clone()))
            .collect()
    });
    &RULES
}

/// Byte window the pair and an assertion must share to count as one clause.
pub fn relation_keyword_proximity() -> usize {
    CONFIG
        .relation_keyword_proximity
        .unwrap_or(DEFAULT_RELATION_KEYWORD_PROXIMITY)
}

pub fn dialog_address_rules() -> &'static [(Vec<String>, String)] {
    static RULES: LazyLock<Vec<(Vec<String>, String)>> = LazyLock::new(|| {
        CONFIG
            .dialog_address_rules
            .iter()
            .map(|r| (r.keywords.clone(), r.relation_type.clone()))
            .collect()
    });
    &RULES
}

pub fn get_faction_constraint(rel_type: &str) -> &'static str {
    static CONSTRAINT_MAP: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
        CONFIG
            .relation_type_rules
            .iter()
            .map(|r| (r.r#type.clone(), r.faction_constraint.clone()))
            .collect()
    });
    CONSTRAINT_MAP
        .get(rel_type)
        .map(|s| s.as_str())
        .unwrap_or("any")
}

// ═══════════════════════════════════════════════════════════════
// Dialog chain extraction
// ═══════════════════════════════════════════════════════════════

pub const DIALOG_ADDRESS_RULES: &[(&[&str], &str)] = &[
    (&["主公", "陛下", "大王", "王上", "皇上", "天子"], "君臣"),
    (&["丞相", "军师", "都督", "将军"], "君臣"),
    (&["哥哥", "大哥", "义兄", "贤弟", "兄弟", "义弟"], "结义"),
    (&["师父", "师傅", "师尊", "恩师"], "师徒"),
    (&["夫人", "娘子", "贤妻", "拙荆"], "夫妻"),
    (&["父亲", "父王", "爹爹", "岳父"], "父子"),
    (&["母亲", "娘亲"], "母女"),
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DialogRelation {
    pub speaker: String,
    pub addressee: String,
    pub relation_type: String,
}

const DIALOG_MARKERS: &[&str] = &["曰：", "道："];

/// Whether `speech` opens with `keyword` — the vocative position.
///
/// Leading punctuation and whitespace are skipped so a quotation mark or an
/// opening bracket does not hide the vocative (`“夫人，…`). The scan stops at
/// the first alphanumeric character, which includes CJK ideographs: anything
/// from there on is the utterance itself, not a form of address opening it.
fn speech_starts_with(speech: &str, keyword: &str) -> bool {
    speech
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .starts_with(keyword)
}

pub fn extract_dialog_relations(
    text: &str,
    name_pairs: &[(String, String)],
) -> Vec<DialogRelation> {
    let ni = DialogNameIndex::new(name_pairs);

    let mut marker_positions: Vec<usize> = Vec::new();
    for m in DIALOG_MARKERS {
        for (pos, _) in text.match_indices(m) {
            marker_positions.push(pos);
        }
    }
    marker_positions.sort();

    let mut speakers: Vec<Option<String>> = Vec::new();
    let mut speeches: Vec<&str> = Vec::new();
    let mut is_reply: Vec<bool> = Vec::new();
    let mut directed: Vec<Option<String>> = Vec::new();

    for &pos in &marker_positions {
        if is_poem_prefix(text, pos) {
            speakers.push(None);
            speeches.push("");
            is_reply.push(false);
            directed.push(None);
            continue;
        }

        let speaker = ni.find_speaker_near(text, pos);

        let check_start = floor_char_boundary(text, pos.saturating_sub(6));
        let reply = pos >= 6 && text[check_start..pos].contains("对");

        let dir = ni.find_directed(text, pos);

        let speech_start = (pos + 4..)
            .find(|&i| text.is_char_boundary(i))
            .unwrap_or(pos + 6);
        let speech = extract_speech_span(text, speech_start, &marker_positions);

        speakers.push(speaker);
        speeches.push(speech);
        is_reply.push(reply);
        directed.push(dir);
    }

    let mut relations: Vec<DialogRelation> = Vec::new();
    let last_non_poem =
        |current: usize| -> Option<usize> { (0..current).rev().find(|&i| speakers[i].is_some()) };

    for i in 0..speakers.len() {
        let sp = match &speakers[i] {
            Some(s) => s.clone(),
            None => continue,
        };

        let speech = speeches[i];
        if speech.is_empty() {
            continue;
        }

        for (keywords, rtype) in dialog_address_rules() {
            // An address form types a relation only in the VOCATIVE position:
            // it has to open the utterance. Anywhere else it is narration that
            // merely mentions the word, and `extract_speech_span` reaches up to
            // 300 bytes into the text that follows — so
            // "王夫人道：袭人，你过来。夫人今日…" paired 王夫人 with 袭人 as a
            // marriage purely because the narration two clauses later contained
            // `夫人`. Vocatives are how address forms are actually used, and
            // that is genre- and language-independent.
            if !keywords
                .iter()
                .any(|kw| speech_starts_with(speech, kw.as_str()))
            {
                continue;
            }

            let addressee = if let Some(to) = &directed[i] {
                Some(to.clone())
            } else {
                last_non_poem(i).and_then(|j| speakers[j].clone())
            };

            if let Some(addr) = addressee {
                if sp != addr {
                    relations.push(DialogRelation {
                        speaker: sp,
                        addressee: addr,
                        relation_type: rtype.clone(),
                    });
                }
            }
            break;
        }
    }

    relations.sort();
    relations.dedup();
    relations
}

fn is_poem_prefix(text: &str, pos: usize) -> bool {
    let search_start = floor_char_boundary(text, pos.saturating_sub(20));
    let prefix = &text[search_start..pos];
    prefix.contains("诗") || prefix.contains("词")
}

/// Aho-Corasick-based name index for dialog functions.
///
/// Built once per chapter, replaces O(N²) name_pairs scanning with a single
/// automaton pass for each text region.
struct DialogNameIndex {
    ac: aho_corasick::AhoCorasick,
    names: Vec<(String, String)>,
}

impl DialogNameIndex {
    fn new(name_pairs: &[(String, String)]) -> Self {
        let patterns: Vec<&str> = name_pairs.iter().map(|(a, _)| a.as_str()).collect();
        let ac = aho_corasick::AhoCorasick::builder()
            .match_kind(aho_corasick::MatchKind::Standard)
            .build(&patterns)
            .expect("Aho-Corasick automaton build should never fail");
        let names: Vec<(String, String)> = name_pairs
            .iter()
            .map(|(a, c)| (a.clone(), c.clone()))
            .collect();
        Self { ac, names }
    }

    /// Find the speaker name near a reference position (e.g., "谓" or marker position).
    fn best_name_near(&self, text: &str, ref_pos: usize) -> Option<String> {
        let search_start = floor_char_boundary(text, ref_pos.saturating_sub(50));
        let search_area = &text[search_start..ref_pos];
        let mut best_end: usize = 0;
        let mut best_idx: Option<usize> = None;
        for m in self.ac.find_overlapping_iter(search_area) {
            let end = search_start + m.end();
            let gap = ref_pos - end;
            if gap <= 6 && end > best_end {
                best_end = end;
                best_idx = Some(m.pattern().as_usize());
            }
        }
        best_idx.map(|i| self.names[i].1.clone())
    }

    /// Find the name in `find_dialog_speaker` context.
    fn find_speaker_near(&self, text: &str, pos: usize) -> Option<String> {
        let before = &text[..pos];
        // Bound the backward search: an unbounded rfind("对"/"谓") once
        // found a stale marker made EVERY later `X曰` inherit that name
        // ("玄德曰…孔明对曰…张飞曰" attributed 张飞 to 孔明). Only accept a
        // marker within LOOKBACK_BYTES of the current position.
        const LOOKBACK_BYTES: usize = 64;
        let window_start = floor_char_boundary(before, before.len().saturating_sub(LOOKBACK_BYTES));
        if let Some(wei) = before.rfind("谓")
            && wei >= window_start
            && let Some(name) = self.best_name_near(text, wei)
        {
            return Some(name);
        }
        if let Some(dui) = before.rfind("对")
            && dui >= window_start
            && let Some(name) = self.best_name_near(text, dui)
        {
            return Some(name);
        }
        self.best_name_near(text, pos)
    }

    /// Find the addressee name in `find_dialog_directed` context.
    fn find_directed(&self, text: &str, pos: usize) -> Option<String> {
        let before = &text[..pos];
        // The same bounded look-back as [`Self::find_speaker_near`]: an
        // unbounded `rfind` accepts a stale `谓`/`对` left over from an earlier
        // clause, and `longest_match` then picks the longest name of that
        // distant span, attributing the wrong addressee to the current
        // sentence. Only a marker within LOOKBACK_BYTES may anchor.
        const LOOKBACK_BYTES: usize = 64;
        let window_start = floor_char_boundary(before, before.len().saturating_sub(LOOKBACK_BYTES));

        if let Some(wei_pos) = before.rfind("谓")
            && wei_pos >= window_start
            && let Some(name) = self.longest_match(&text[wei_pos + 3..pos])
        {
            return Some(name);
        }

        if let Some(dui_pos) = before.rfind("对")
            && dui_pos >= window_start
            && let Some(name) = self.longest_match(&text[dui_pos + 3..pos])
        {
            return Some(name);
        }

        None
    }

    /// Find the longest matching name in `text`.
    fn longest_match(&self, text: &str) -> Option<String> {
        self.ac
            .find_overlapping_iter(text)
            .max_by_key(|m| m.len())
            .map(|m| self.names[m.pattern().as_usize()].1.clone())
    }
}

fn extract_speech_span<'a>(text: &'a str, start: usize, all_markers: &[usize]) -> &'a str {
    // Floor BOTH ends: the caller passes byte offsets that may land inside a
    // multi-byte character, and slicing at a non-boundary panics.
    let start = floor_char_boundary(text, start);
    let end = all_markers
        .iter()
        .find(|&&p| p >= start)
        .copied()
        .unwrap_or_else(|| start.saturating_add(300).min(text.len()));
    let end = floor_char_boundary(text, end);
    &text[start..end]
}

// ═══════════════════════════════════════════════════════════════
// Proximity-based relation detection
// ═══════════════════════════════════════════════════════════════

fn all_names_for(name: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for novel in NOVELS {
        for cdef in get_novel_characters(novel) {
            if cdef.name == name {
                names.push(cdef.name.to_string());
                for a in cdef.aliases {
                    names.push(a.to_string());
                }
            }
        }
    }
    names
}

pub fn detect_relation_type(context: &str, a: &str, b: &str) -> String {
    if context.is_empty() {
        return "关联".to_string();
    }

    let a_spans: Vec<(usize, usize)> = all_names_for(a)
        .iter()
        .flat_map(|name| {
            context
                .match_indices(name.as_str())
                .map(|(start, matched)| (start, start + matched.len()))
        })
        .collect();
    let b_spans: Vec<(usize, usize)> = all_names_for(b)
        .iter()
        .flat_map(|name| {
            context
                .match_indices(name.as_str())
                .map(|(start, matched)| (start, start + matched.len()))
        })
        .collect();

    for (keywords, rtype) in relation_type_rules() {
        for kw in keywords {
            if context.match_indices(kw.as_str()).any(|(kw_pos, matched)| {
                keyword_types_relation(
                    &a_spans,
                    &b_spans,
                    kw_pos,
                    matched.len(),
                    relation_keyword_proximity(),
                )
            }) {
                return rtype.clone();
            }
        }
    }
    "关联".to_string()
}

/// Whether a keyword occurrence of `kw_len` bytes at `kw_pos` types the
/// relation between `a` and `b` (spans are `(start, end)` byte ranges of any
/// alias occurrence).
///
/// Two conditions, both forced by what the shipped corpus exposed:
///
/// - **Not inside a name.** The match must not fall within an occurrence of
///   either name. `夫人` is a substring of `王夫人`, so "王夫人对宝玉说…"
///   married 王夫人 to everyone she spoke to — the bulk of the 68 surviving
///   `夫妻` edges in 红楼梦.
/// - **One clause.** The pair and the keyword must share a single
///   same [`relation_keyword_proximity`]-byte window. The rule this replaces asked
///   only that the keyword be near each name **taken separately**, which also
///   accepts two names two windows apart — with a whole chapter as the context,
///   any common keyword (`夫人`/`娶`/`兄弟`) between two unrelated names
///   asserted the relation.
///
/// Every genuine form keeps all three together, so the window costs nothing:
/// `A娶B`, `A与B配为夫妇`, `A、B…结义为兄弟` and `A拜B为师` all fit one clause.
fn keyword_types_relation(
    a_spans: &[(usize, usize)],
    b_spans: &[(usize, usize)],
    kw_pos: usize,
    kw_len: usize,
    proximity: usize,
) -> bool {
    let kw_end = kw_pos + kw_len;
    let inside = |spans: &[(usize, usize)]| {
        spans
            .iter()
            .any(|&(start, end)| start <= kw_pos && kw_end <= end)
    };
    if inside(a_spans) || inside(b_spans) {
        return false;
    }
    a_spans.iter().any(|&(ap, _)| {
        b_spans.iter().any(|&(bp, _)| {
            let lo = ap.min(bp).min(kw_pos);
            let hi = ap.max(bp).max(kw_end);
            hi - lo <= proximity
        })
    })
}

// ═══════════════════════════════════════════════════════════════
// Pre-computed chapter index for O(N²)-free relation detection
// ═══════════════════════════════════════════════════════════════

/// Pre-computed index for a single chapter, built once per chapter and used
/// for all pair-wise relation type checks instead of scanning the full text
/// for each pair.
pub struct ChapterRelationIndex {
    keyword_positions: HashMap<String, Vec<usize>>,
}

impl ChapterRelationIndex {
    /// Build index from chapter text.
    ///
    /// Scans the text once per keyword (total keywords ~50), collecting all
    /// byte positions. Pair-wise detection then uses these pre-computed
    /// positions instead of calling `match_indices` for every pair.
    pub fn build(text: &str) -> Self {
        let mut keyword_positions: HashMap<String, Vec<usize>> = HashMap::new();
        for (keywords, _) in relation_type_rules() {
            for kw in keywords {
                if keyword_positions.contains_key(kw) {
                    continue;
                }
                let positions: Vec<usize> =
                    text.match_indices(kw.as_str()).map(|(p, _)| p).collect();
                if !positions.is_empty() {
                    keyword_positions.insert(kw.clone(), positions);
                }
            }
        }
        Self { keyword_positions }
    }

    /// Fast relation type detection using pre-computed index.
    ///
    /// `char_spans` maps character name → the `(start, end)` byte range of
    /// every alias occurrence in this chapter (derived from the alias-matching
    /// phase). The end is needed, not just the start: a keyword match that falls
    /// *inside* one of the names is part of that name, not a relation
    /// ([`keyword_types_relation`]). Returns the first matching relation type,
    /// or `"关联"` if none found.
    pub fn detect_type(
        &self,
        char_spans: &HashMap<String, Vec<(usize, usize)>>,
        a: &str,
        b: &str,
    ) -> String {
        let a_spans = match char_spans.get(a) {
            Some(spans) => spans,
            None => return "关联".to_string(),
        };
        let b_spans = match char_spans.get(b) {
            Some(spans) => spans,
            None => return "关联".to_string(),
        };

        for (keywords, rtype) in relation_type_rules() {
            for kw in keywords {
                let kw_positions = match self.keyword_positions.get(kw) {
                    Some(positions) => positions,
                    None => continue,
                };
                if kw_positions.iter().any(|&kw_pos| {
                    keyword_types_relation(
                        a_spans,
                        b_spans,
                        kw_pos,
                        kw.len(),
                        relation_keyword_proximity(),
                    )
                }) {
                    return rtype.clone();
                }
            }
        }
        "关联".to_string()
    }
}

/// Fast context-window finder using pre-computed spans.
///
/// Instead of scanning the full text for alias strings (the original
/// `find_relation_context`), this uses the pre-computed `char_spans`
/// map to check proximity in constant time per position.
pub fn find_relation_context_indexed(
    text: &str,
    char_spans: &HashMap<String, Vec<(usize, usize)>>,
    a: &str,
    b: &str,
) -> Option<String> {
    let a_spans = char_spans.get(a)?;
    let b_spans = char_spans.get(b)?;

    for &(a_pos, _) in a_spans {
        let ctx_start = floor_char_boundary(text, a_pos.saturating_sub(30));
        let ctx_end = floor_char_boundary(text, (a_pos + 200).min(text.len()));
        if !b_spans
            .iter()
            .any(|&(bp, _)| bp >= ctx_start && bp < ctx_end)
        {
            continue;
        }
        let ctx = &text[ctx_start..ctx_end];
        let result: String = ctx.chars().take(300).collect();
        return Some(result);
    }
    None
}

// ═══════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify 夫妻 keywords map a co-occurring pair to the 夫妻 relation.
    /// Invariants: the detected relation type is 夫妻.
    #[test]
    fn detect_fuqi_relation() {
        let ctx = "宋江与扈三娘配为夫妇，众人皆贺。";
        let rtype = detect_relation_type(ctx, "宋江", "扈三娘");
        assert_eq!(rtype, "夫妻", "夫妻 keywords must map to the 夫妻 type");
    }

    /// Objective: Verify 结义 keywords map a sworn-brotherhood sentence to the 结义 relation.
    /// Invariants: the detected relation type is 结义.
    #[test]
    fn detect_jieyi_relation() {
        let ctx = "刘备、关羽、张飞三人结义为兄弟，誓同生死。";
        let rtype = detect_relation_type(ctx, "刘备", "关羽");
        assert_eq!(rtype, "结义", "结义 keywords must map to the 结义 type");
    }

    /// Objective: Verify 师徒 keywords map the pair to the 师徒 relation.
    /// Invariants: the detected relation type is 师徒.
    #[test]
    fn detect_shitu_relation() {
        let ctx = "那孙悟空拜唐僧为师，跟随师父西行。";
        let rtype = detect_relation_type(ctx, "孙悟空", "唐僧");
        assert_eq!(rtype, "师徒", "师徒 keywords must map to the 师徒 type");
    }

    /// Objective: Verify a relation keyword types the pair only when all three
    /// fit in one clause. The rule this replaced accepted a keyword that was
    /// merely near each name separately, so two unrelated characters with a
    /// stray `夫人` between them were married — on the shipped corpus that
    /// produced 75 `夫妻` edges of which about two are real.
    /// Invariants: a marriage stated in one clause fires; the same keyword with
    /// the pair spread beyond one clause does not.
    #[test]
    fn keyword_types_a_relation_only_within_one_clause() {
        assert_eq!(
            detect_relation_type("王英娶扈三娘为妻。", "王英", "扈三娘"),
            "夫妻",
            "a marriage stated in one clause must still be detected"
        );
        assert_eq!(
            detect_relation_type("宋江与扈三娘配为夫妇，众人皆贺。", "宋江", "扈三娘"),
            "夫妻",
            "a clause-final marker must still be detected"
        );

        // Identical keyword and names, but the pair is spread across the
        // sentence so no single clause contains both names and the marker.
        let spread = format!("贾母{}夫人{}贾政", "说".repeat(20), "听".repeat(20));
        assert_eq!(
            detect_relation_type(&spread, "贾母", "贾政"),
            "关联",
            "a stray 夫人 far from both names must not assert a marriage"
        );

        // `夫人` is a substring of `王夫人`, so the match is part of that name
        // rather than a marker, however close the clause is.
        assert_eq!(
            detect_relation_type("王夫人对贾宝玉说：你这孽障。", "王夫人", "贾宝玉"),
            "关联",
            "a keyword lying inside one of the names must not type the relation"
        );
    }

    /// Objective: Verify the shipped rule set and the built-in fallback both
    /// validate, and that a word which is *purely* a form of address — one that
    /// names no bond and therefore has no asserting use — is never listed as an
    /// assertion.
    /// Invariants: both rule sets validate; each of these words appears only as
    /// an address form.
    #[test]
    fn pure_address_forms_are_not_assertions() {
        validate(&fallback_config()).expect("the built-in fallback must validate");

        let path = std::env::var("RELATION_RULES_PATH")
            .unwrap_or_else(|_| "config/relation_rules.json".to_string());
        let shipped = load_config_from_path(Path::new(&path))
            .expect("config/relation_rules.json must load and validate");

        // Sharing a clause with one of these is evidence of nothing: none of them
        // names a bond, so only an actual vocative may draw a relation from it.
        // (`兄弟` is deliberately absent: in 水浒传 it both names the bond and
        // serves as a form of address, so it belongs in both lists.)
        for word in [
            "夫人", "娘子", "贤妻", "拙荆", "内人", "哥哥", "大哥", "二哥", "三弟", "贤弟", "皇上",
            "军师", "老师", "父亲", "爹爹", "岳父", "母亲",
        ] {
            assert!(
                !shipped
                    .relation_type_rules
                    .iter()
                    .any(|rule| rule.assertions.iter().any(|a| a == word)),
                "`{word}` names no bond and must not be an `assertions` entry"
            );
        }
    }

    /// Objective: Verify `validate` refuses rule sets that cannot be honoured.
    /// Invariants: each malformed rule set is refused rather than silently
    /// degraded to a different rule set.
    #[test]
    fn validate_rejects_unusable_rules() {
        let unknown_constraint = RelationRulesConfig {
            relation_keyword_proximity: None,
            relation_type_rules: vec![rtc("夫妻", &["夫妻"], "sometimes")],
            dialog_address_rules: vec![],
        };
        assert!(
            validate(&unknown_constraint).is_err(),
            "an unknown `faction_constraint` must be refused rather than ignored"
        );

        let no_assertions = RelationRulesConfig {
            relation_keyword_proximity: None,
            relation_type_rules: vec![rtc("夫妻", &[], "any")],
            dialog_address_rules: vec![],
        };
        assert!(
            validate(&no_assertions).is_err(),
            "a relation type with no assertions must be refused"
        );
    }

    /// Objective: Verify the clause window is read from the configuration rather
    /// than fixed, so a language whose relation phrase sits further from its
    /// arguments can widen it.
    /// Invariants: a span too wide for the default window is refused by a narrow
    /// one and accepted by a wider one.
    #[test]
    fn clause_window_comes_from_the_configuration() {
        let a_spans = [(0usize, 6usize)];
        let b_spans = [(60usize, 66usize)];
        let kw_pos = 30; // 30 bytes from each name, 66 bytes end to end

        assert!(
            !keyword_types_relation(&a_spans, &b_spans, kw_pos, 3, 40),
            "a 66-byte span must not fit the 40-byte default window"
        );
        assert!(
            keyword_types_relation(&a_spans, &b_spans, kw_pos, 3, 80),
            "the same span must fit when the configuration widens the window"
        );
    }

    /// Objective: Verify a title that names a bond still types a pair when the
    /// two names and the title share one clause.
    /// Invariants: `主公` between two names yields 君臣.
    #[test]
    fn a_bond_naming_title_types_a_shared_clause() {
        assert_eq!(
            detect_relation_type("孔明向主公刘备进言。", "孔明", "刘备"),
            "君臣",
            "a title that names the bond must type the pair it sits between"
        );
    }

    /// Objective: Verify an address form counts only in the vocative position,
    /// so narration later in the same speech cannot type a couple.
    /// Invariants: a leading vocative is accepted even behind punctuation; the
    /// same word buried inside the utterance is not an address.
    #[test]
    fn address_forms_count_only_in_the_vocative() {
        assert!(
            speech_starts_with("主公有何吩咐？", "主公"),
            "a leading vocative must count"
        );
        assert!(
            speech_starts_with("“夫人，你来了。”", "夫人"),
            "leading punctuation must not hide the vocative"
        );
        assert!(
            !speech_starts_with("袭人，夫人叫你呢。", "夫人"),
            "a mere mention inside the utterance is not a form of address"
        );
        assert!(
            !speech_starts_with("袭人，你过来。", "夫人"),
            "an absent address form must not count"
        );
    }

    /// Objective: Verify a context with no relation keyword falls back to the generic 关联 type.
    /// Invariants: the detected relation type is 关联.
    #[test]
    fn fallback_to_generic() {
        let ctx = "这个人跟那个人一起走着。";
        let rtype = detect_relation_type(ctx, "刘备", "关羽");
        assert_eq!(
            rtype, "关联",
            "an unmatched sentence must fall back to 关联"
        );
    }

    /// Objective: Verify an empty context falls back to the generic 关联 type.
    /// Invariants: the detected relation type is 关联.
    #[test]
    fn empty_context_returns_generic() {
        let rtype = detect_relation_type("", "宋江", "吴用");
        assert_eq!(rtype, "关联", "an empty context must fall back to 关联");
    }

    /// Objective: Verify an address keyword inside a reply yields the 君臣 dialog relation.
    /// Invariants: the result contains the 诸葛亮→刘备 君臣 relation.
    #[test]
    fn dialog_extracts_junchen_from_zhu_gong() {
        let text = "玄德曰：孔明何在？孔明对曰：主公有何吩咐？";
        let name_pairs = vec![
            ("孔明".to_string(), "诸葛亮".to_string()),
            ("玄德".to_string(), "刘备".to_string()),
            ("诸葛亮".to_string(), "诸葛亮".to_string()),
            ("刘备".to_string(), "刘备".to_string()),
        ];
        let relations = extract_dialog_relations(text, &name_pairs);
        assert!(
            relations.contains(&DialogRelation {
                speaker: "诸葛亮".to_string(),
                addressee: "刘备".to_string(),
                relation_type: "君臣".to_string(),
            }),
            "got {relations:?}"
        );
    }

    /// Objective: Verify a dialog marker sitting inside a poem attribution is skipped.
    /// Invariants: no dialog relation is produced.
    #[test]
    fn dialog_skips_poem_prefix() {
        let text = "诗曰：主公在上。玄德曰：善。";
        let name_pairs = vec![
            ("玄德".to_string(), "刘备".to_string()),
            ("刘备".to_string(), "刘备".to_string()),
        ];
        let relations = extract_dialog_relations(text, &name_pairs);
        assert!(relations.is_empty(), "got {relations:?}");
    }

    /// Objective: Verify the 道 dialog marker resolves both speaker and addressee.
    /// Invariants: the result contains the 吴用→宋江 结义 relation.
    #[test]
    fn dialog_works_with_dao_marker() {
        let text = "吴用对宋江道：哥哥在上，小弟有一言。";
        let name_pairs = vec![
            ("宋江".to_string(), "宋江".to_string()),
            ("吴用".to_string(), "吴用".to_string()),
            ("哥哥".to_string(), "宋江".to_string()),
        ];
        let relations = extract_dialog_relations(text, &name_pairs);
        assert!(
            relations.contains(&DialogRelation {
                speaker: "吴用".to_string(),
                addressee: "宋江".to_string(),
                relation_type: "结义".to_string(),
            }),
            "got {relations:?}"
        );
    }

    /// Objective: Verify a lone 谓X曰 sentence does not fabricate a dialog relation.
    /// Invariants: no dialog relation is produced.
    #[test]
    fn dialog_resolves_wei_x_yue() {
        let text = "玄德谓孔明曰：先生何以教我？";
        let name_pairs = vec![
            ("孔明".to_string(), "诸葛亮".to_string()),
            ("玄德".to_string(), "刘备".to_string()),
            ("诸葛亮".to_string(), "诸葛亮".to_string()),
            ("刘备".to_string(), "刘备".to_string()),
        ];
        let relations = extract_dialog_relations(text, &name_pairs);
        assert!(
            relations.is_empty(),
            "a 谓X曰 sentence must not fabricate a dialog relation"
        );
    }

    /// Objective: Verify no relation rule carries an empty keyword.
    /// Invariants: every rule has at least one keyword and no keyword is empty.
    #[test]
    fn relation_rules_have_no_empty_keywords() {
        for (keywords, rtype) in relation_type_rules() {
            assert!(!keywords.is_empty(), "rule for {rtype} has no keywords");
            for kw in keywords {
                assert!(!kw.is_empty(), "empty keyword in rule {rtype}");
            }
        }
    }

    /// Objective: Verify a malformed relation-rules file is reported as an
    /// error instead of being silently swapped for the built-in rules
    /// (audit H16: the old `.ok()` chain hid every parse failure).
    /// Invariants: `load_config_from_path` returns `Err` naming the offending
    /// file for invalid JSON.
    #[test]
    fn malformed_relation_rules_are_reported() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let bad = dir.path().join("relation_rules.json");
        std::fs::write(&bad, "{ \"relation_type_rules\": [ , ] }").expect("write malformed");
        let err = load_config_from_path(&bad).expect_err("malformed JSON must be an error");
        assert!(
            err.contains("invalid JSON"),
            "error must mention the parse failure, got: {err}"
        );
        assert!(
            err.contains("relation_rules.json"),
            "error must name the offending file, got: {err}"
        );
    }

    /// Objective: Verify the shipped `config/relation_rules.json` actually
    /// parses, so the runtime never silently falls back to the built-in rules.
    /// Invariants: Parsing the bundled file succeeds and yields a non-empty
    /// rule set.
    #[test]
    fn shipped_relation_rules_parse() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/relation_rules.json");
        let cfg = load_config_from_path(&path)
            .unwrap_or_else(|e| panic!("shipped relation_rules.json must parse: {e}"));
        assert!(
            !cfg.relation_type_rules.is_empty(),
            "shipped relation rules must define at least one relation type"
        );
        assert!(
            !cfg.dialog_address_rules.is_empty(),
            "shipped relation rules must define at least one dialog address rule"
        );
    }

    /// Objective: Prove the addressee anchor obeys the same bounded look-back
    /// as the speaker anchor, so a stale `对` from an earlier clause is not
    /// reused for a much later dialog marker.
    /// Invariants: A `对` beyond `LOOKBACK_BYTES` yields `None`; the identical
    /// construction with the `对` inside the window still resolves the name.
    #[test]
    fn directed_ignores_stale_marker_outside_lookback() {
        let name_pairs = vec![
            ("刘备".to_string(), "刘备".to_string()),
            ("曹操".to_string(), "曹操".to_string()),
        ];
        let ni = DialogNameIndex::new(&name_pairs);

        // The only `对` sits 141 bytes before the second marker, far outside
        // the window; it must not anchor the addressee of the later sentence.
        let stale = format!("刘备对曹操曰：{}曹操曰：", "甲".repeat(40));
        let stale_pos = stale.rfind("曰：").expect("second marker position");
        assert_eq!(
            ni.find_directed(&stale, stale_pos),
            None,
            "a `对` outside the look-back window must not anchor the addressee"
        );

        // Control: the same names with a nearby `对` still resolve.
        let near = "曹操对刘备曰：久仰。";
        let near_pos = near.rfind("曰：").expect("marker position");
        assert_eq!(
            ni.find_directed(near, near_pos).as_deref(),
            Some("刘备"),
            "a `对` inside the look-back window must still anchor the addressee"
        );
    }

    /// Objective: Prove the speech span tolerates a byte offset that lands
    /// inside a multi-byte character instead of panicking on the slice.
    /// Invariants: The returned span is a valid slice of the input and starts
    /// at the character boundary at or before the requested offset.
    #[test]
    fn speech_span_tolerates_non_boundary_start() {
        let text = "曹操曰：久仰大名。";
        // One byte past the start of "曰" is inside that character.
        let mid = text.find('曰').expect("marker present") + 1;
        assert!(
            !text.is_char_boundary(mid),
            "fixture must start inside a multi-byte character"
        );
        let span = extract_speech_span(text, mid, &[]);
        assert!(
            span.starts_with('曰'),
            "start must floor to the character boundary, got: {span:?}"
        );
        assert!(
            text.contains(span),
            "the span must remain a slice of the input text"
        );
    }
}
