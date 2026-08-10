use std::collections::HashMap;
use std::sync::LazyLock;

use super::characters::{NOVELS, get_novel_characters};
use super::extract::floor_char_boundary;
use serde::Deserialize;

// ═══════════════════════════════════════════════════════════════
// JSON config types
// ═══════════════════════════════════════════════════════════════

#[derive(Deserialize, Clone)]
struct RelationRulesConfig {
    relation_type_rules: Vec<RelationTypeRuleConfig>,
    dialog_address_rules: Vec<DialogAddressRuleConfig>,
}

#[derive(Deserialize, Clone)]
struct RelationTypeRuleConfig {
    #[serde(rename = "type")]
    r#type: String,
    keywords: Vec<String>,
    faction_constraint: String,
}

#[derive(Deserialize, Clone)]
struct DialogAddressRuleConfig {
    keywords: Vec<String>,
    relation_type: String,
}

fn load_config() -> RelationRulesConfig {
    let path = crate::config::resolve_resource_path("config/relation_rules.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(fallback_config)
}

static CONFIG: LazyLock<RelationRulesConfig> = LazyLock::new(load_config);

/// Builtin fallback rules used when no JSON config is found.
fn fallback_config() -> RelationRulesConfig {
    RelationRulesConfig {
        relation_type_rules: vec![
            rtc("夫妻", &["夫妻", "夫妇", "配为夫妇", "嫁与", "娶"], "any"),
            rtc("结义", &["结义", "兄弟", "义兄", "义妹", "拜为兄弟"], "any"),
            rtc("父子", &["父子", "父女"], "any"),
            rtc("母女", &["母子", "母女"], "any"),
            rtc("师徒", &["师徒", "师父", "徒弟", "拜师"], "same"),
            rtc("君臣", &["君臣", "丞相", "陛下"], "same"),
            rtc("挚友", &["挚友", "好友"], "any"),
            rtc("仇敌", &["仇敌", "仇人", "对头"], "different"),
            rtc("姐妹", &["姐妹", "姊妹"], "any"),
            rtc("亲戚", &["亲戚", "亲属", "亲家"], "any"),
        ],
        dialog_address_rules: vec![
            dac(&["主公", "陛下", "大王", "王上", "皇上", "天子"], "君臣"),
            dac(&["丞相", "军师", "都督", "将军"], "君臣"),
            dac(&["哥哥", "大哥", "义兄", "贤弟", "兄弟", "义弟"], "结义"),
            dac(&["师父", "师傅", "师尊", "恩师"], "师徒"),
            dac(&["夫人", "娘子", "贤妻", "拙荆"], "夫妻"),
            dac(&["父亲", "父王", "爹爹", "岳父"], "父子"),
            dac(&["母亲", "娘亲"], "母女"),
        ],
    }
}

fn rtc(r#type: &str, keywords: &[&str], faction_constraint: &str) -> RelationTypeRuleConfig {
    RelationTypeRuleConfig {
        r#type: r#type.to_string(),
        keywords: keywords.iter().map(|s| s.to_string()).collect(),
        faction_constraint: faction_constraint.to_string(),
    }
}

fn dac(keywords: &[&str], relation_type: &str) -> DialogAddressRuleConfig {
    DialogAddressRuleConfig {
        keywords: keywords.iter().map(|s| s.to_string()).collect(),
        relation_type: relation_type.to_string(),
    }
}

// ═══════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════

pub fn relation_type_rules() -> &'static [(Vec<String>, String)] {
    static RULES: LazyLock<Vec<(Vec<String>, String)>> = LazyLock::new(|| {
        CONFIG
            .relation_type_rules
            .iter()
            .map(|r| (r.keywords.clone(), r.r#type.clone()))
            .collect()
    });
    &RULES
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
            let kw_found = keywords.iter().any(|kw| speech.contains(kw.as_str()));
            if !kw_found {
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
        if let Some(wei) = before.rfind("谓") {
            if let Some(name) = self.best_name_near(text, wei) {
                return Some(name);
            }
        }
        if let Some(dui) = before.rfind("对") {
            if let Some(name) = self.best_name_near(text, dui) {
                return Some(name);
            }
            return self.best_name_near(text, dui);
        }
        self.best_name_near(text, pos)
    }

    /// Find the addressee name in `find_dialog_directed` context.
    fn find_directed(&self, text: &str, pos: usize) -> Option<String> {
        let before = &text[..pos];

        if let Some(wei_pos) = before.rfind("谓") {
            let between = &text[wei_pos + 3..pos];
            if let Some(name) = self.longest_match(between) {
                return Some(name);
            }
        }

        if let Some(dui_pos) = before.rfind("对") {
            let between = &text[dui_pos + 3..pos];
            if let Some(name) = self.longest_match(between) {
                return Some(name);
            }
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
    let start = start.min(text.len());
    let end = all_markers
        .iter()
        .find(|&&p| p >= start)
        .copied()
        .unwrap_or((start + 300).min(text.len()));
    let end = floor_char_boundary(text, end);
    &text[start..end]
}

// ═══════════════════════════════════════════════════════════════
// Proximity-based relation detection
// ═══════════════════════════════════════════════════════════════

const RELATION_KEYWORD_PROXIMITY: usize = 40;

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

    let a_names = all_names_for(a);
    let b_names = all_names_for(b);

    for (keywords, rtype) in relation_type_rules() {
        for kw in keywords {
            if !context.contains(kw.as_str()) {
                continue;
            }
            for (kw_pos, _) in context.match_indices(kw.as_str()) {
                let a_near = a_names.iter().any(|an| {
                    context.match_indices(an.as_str()).any(|(p, _)| {
                        (p as i32 - kw_pos as i32).unsigned_abs()
                            < RELATION_KEYWORD_PROXIMITY as u32
                    })
                });
                let b_near = b_names.iter().any(|bn| {
                    context.match_indices(bn.as_str()).any(|(p, _)| {
                        (p as i32 - kw_pos as i32).unsigned_abs()
                            < RELATION_KEYWORD_PROXIMITY as u32
                    })
                });

                if a_near && b_near {
                    return rtype.clone();
                }
            }
        }
    }
    "关联".to_string()
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
    /// `char_positions` maps character name → all byte positions of any alias
    /// in this chapter (derived from the alias-matching phase). Returns the
    /// first matching relation type, or `"关联"` if none found.
    pub fn detect_type(
        &self,
        char_positions: &HashMap<String, Vec<usize>>,
        a: &str,
        b: &str,
    ) -> String {
        let a_positions = match char_positions.get(a) {
            Some(p) => p,
            None => return "关联".to_string(),
        };
        let b_positions = match char_positions.get(b) {
            Some(p) => p,
            None => return "关联".to_string(),
        };

        for (keywords, rtype) in relation_type_rules() {
            for kw in keywords {
                let kw_positions = match self.keyword_positions.get(kw) {
                    Some(p) => p,
                    None => continue,
                };
                for &kw_pos in kw_positions {
                    let a_near = a_positions.iter().any(|&p| {
                        (p as i32 - kw_pos as i32).unsigned_abs()
                            < RELATION_KEYWORD_PROXIMITY as u32
                    });
                    if !a_near {
                        continue;
                    }
                    let b_near = b_positions.iter().any(|&p| {
                        (p as i32 - kw_pos as i32).unsigned_abs()
                            < RELATION_KEYWORD_PROXIMITY as u32
                    });
                    if b_near {
                        return rtype.clone();
                    }
                }
            }
        }
        "关联".to_string()
    }
}

/// Fast context-window finder using pre-computed positions.
///
/// Instead of scanning the full text for alias strings (the original
/// `find_relation_context`), this uses the pre-computed `char_positions`
/// map to check proximity in constant time per position.
pub fn find_relation_context_indexed(
    text: &str,
    char_positions: &HashMap<String, Vec<usize>>,
    a: &str,
    b: &str,
) -> Option<String> {
    let a_positions = char_positions.get(a)?;
    let b_positions = char_positions.get(b)?;

    for &a_pos in a_positions {
        let ctx_start = floor_char_boundary(text, a_pos.saturating_sub(30));
        let ctx_end = floor_char_boundary(text, (a_pos + 200).min(text.len()));
        if !b_positions
            .iter()
            .any(|&bp| bp >= ctx_start && bp < ctx_end)
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

    #[test]
    fn detect_fuqi_relation() {
        let ctx = "宋江与扈三娘配为夫妇，众人皆贺。";
        let rtype = detect_relation_type(ctx, "宋江", "扈三娘");
        assert_eq!(rtype, "夫妻");
    }

    #[test]
    fn detect_jieyi_relation() {
        let ctx = "刘备、关羽、张飞三人结义为兄弟，誓同生死。";
        let rtype = detect_relation_type(ctx, "刘备", "关羽");
        assert_eq!(rtype, "结义");
    }

    #[test]
    fn detect_shitu_relation() {
        let ctx = "那孙悟空拜唐僧为师，跟随师父西行。";
        let rtype = detect_relation_type(ctx, "孙悟空", "唐僧");
        assert_eq!(rtype, "师徒");
    }

    #[test]
    fn fallback_to_generic() {
        let ctx = "这个人跟那个人一起走着。";
        let rtype = detect_relation_type(ctx, "刘备", "关羽");
        assert_eq!(rtype, "关联");
    }

    #[test]
    fn empty_context_returns_generic() {
        let rtype = detect_relation_type("", "宋江", "吴用");
        assert_eq!(rtype, "关联");
    }

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
        assert!(relations.is_empty());
    }

    #[test]
    fn relation_rules_have_no_empty_keywords() {
        for (keywords, rtype) in relation_type_rules() {
            assert!(!keywords.is_empty(), "rule for {rtype} has no keywords");
            for kw in keywords {
                assert!(!kw.is_empty(), "empty keyword in rule {rtype}");
            }
        }
    }
}
