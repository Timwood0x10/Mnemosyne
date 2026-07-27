/// Relation type detection from context text.
use super::characters::{NOVELS, get_novel_characters};
use super::extract::floor_char_boundary;

/// Address-term keywords → relation type mapping for dialog chain extraction.
///
/// Used by [`extract_dialog_relations`] to detect directed relations when
/// a character addresses another by a role keyword (e.g. "主公" → 君臣).
/// The caller must also resolve who the addressee is from the dialog chain.
pub const DIALOG_ADDRESS_RULES: &[(&[&str], &str)] = &[
    (&["主公", "陛下", "大王", "王上", "皇上", "天子"], "君臣"),
    (&["丞相", "军师", "都督", "将军"], "君臣"),
    (&["哥哥", "大哥", "义兄", "贤弟", "兄弟", "义弟"], "结义"),
    (&["师父", "师傅", "师尊", "恩师"], "师徒"),
    (&["夫人", "娘子", "贤妻", "拙荆"], "夫妻"),
    (&["父亲", "父王", "爹爹", "岳父"], "父子"),
    (&["母亲", "娘亲"], "母女"),
];

/// A relation extracted from dialog chain analysis.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DialogRelation {
    pub speaker: String,
    pub addressee: String,
    pub relation_type: String,
}

/// Dialog markers: the speech-introducing verb followed by fullwidth colon.
/// 曰： is used in 三国演义; 道： is used in 水浒传/西游记/红楼梦.
const DIALOG_MARKERS: &[&str] = &["曰：", "道："];

/// Extract directed relations from a chapter's dialog chain.
///
/// Parses every `曰：` / `道：` occurrence, builds a dialog chain, identifies
/// address keywords (主公/哥哥/师父/陛下 etc.) in speech content, and resolves
/// the addressee from dialog context — supporting `对曰/对道` (reply → previous
/// speaker) and `谓X曰/谓X道` (explicit addressee).
///
/// `name_pairs` is `(alias, canonical)` sorted by alias length descending.
pub fn extract_dialog_relations(
    text: &str,
    name_pairs: &[(String, String)],
) -> Vec<DialogRelation> {
    // Collect all dialog marker positions, sorted
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

        // Speaker: nearest character name before the dialog marker
        let speaker = find_dialog_speaker(text, pos, name_pairs);

        // Detect 对曰/对道 reply: check if "对" is immediately before the verb.
        // Use floor_char_boundary in case the preceding character is ASCII (e.g. newline).
        let check_start = floor_char_boundary(text, pos.saturating_sub(6));
        let reply = pos >= 6 && text[check_start..pos].contains("对");

        // Detect 谓X曰/谓X道 directed addressee
        let dir = find_dialog_directed(text, pos, name_pairs);

        // Speech: from after the marker (verb + colon) to next marker.
        // The colon may be fullwidth (3 bytes) or ASCII (1 byte), so find the
        // next char boundary after the verb instead of hardcoding +6.
        let speech_start = (pos + 4..)
            .find(|&i| text.is_char_boundary(i))
            .unwrap_or(pos + 6);
        let speech = extract_speech_span(text, speech_start, &marker_positions);

        speakers.push(speaker);
        speeches.push(speech);
        is_reply.push(reply);
        directed.push(dir);
    }

    // Second pass: resolve addressees from dialog chain
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

        for &(keywords, rtype) in DIALOG_ADDRESS_RULES {
            let kw_found = keywords.iter().any(|kw| speech.contains(kw));
            if !kw_found {
                continue;
            }

            let addressee = if let Some(to) = &directed[i] {
                Some(to.clone())
            } else if is_reply[i] {
                last_non_poem(i).and_then(|j| speakers[j].clone())
            } else {
                last_non_poem(i).and_then(|j| speakers[j].clone())
            };

            if let Some(addr) = addressee {
                if sp != addr {
                    relations.push(DialogRelation {
                        speaker: sp,
                        addressee: addr,
                        relation_type: rtype.to_string(),
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

/// Check if a dialog marker at `pos` is a poetry marker (诗曰/诗道/有诗曰 etc.).
///
/// Uses a safe char-bounded slice to avoid panicking on multi-byte boundaries.
fn is_poem_prefix(text: &str, pos: usize) -> bool {
    let search_start = floor_char_boundary(text, pos.saturating_sub(20));
    let prefix = &text[search_start..pos];
    prefix.contains("诗") || prefix.contains("词")
}

/// Find the speaker (canonical name) for a dialog marker at `pos`.
///
/// For `谓X曰/谓X道` and `对X曰/对X道` patterns, the speaker is the name
/// BEFORE 谓/对, not the name closest to the marker. This handles:
///   - 玄德谓孔明曰 → speaker=玄德 (孔明 is addressee)
///   - 吴用对宋江道 → speaker=吴用 (宋江 is addressee)
/// Falls back to the nearest name before the marker for plain `曰：`/`道：`.
fn find_dialog_speaker(text: &str, pos: usize, name_pairs: &[(String, String)]) -> Option<String> {
    let before = &text[..pos];

    // 谓X曰/谓X道: speaker is the name before 谓
    if let Some(wei) = before.rfind("谓") {
        let between = &text[wei + 3..pos];
        let name_between = name_pairs
            .iter()
            .any(|(alias, _)| between.contains(alias.as_str()));
        if name_between {
            return find_name_near(text, wei, name_pairs);
        }
    }

    // 对X曰/对X道: speaker is the name before 对
    if let Some(dui) = before.rfind("对") {
        let between = &text[dui + 3..pos];
        let name_between = name_pairs
            .iter()
            .any(|(alias, _)| between.contains(alias.as_str()));
        if name_between {
            return find_name_near(text, dui, name_pairs);
        }
        // 对曰/对道 (reply, no explicit addressee): speaker is still before 对
        return find_name_near(text, dui, name_pairs);
    }

    // Plain 曰：/道：: nearest name before the marker
    find_name_near(text, pos, name_pairs)
}

/// Find the character name whose occurrence ends closest to `ref_pos` (within 50 bytes).
fn find_name_near(text: &str, ref_pos: usize, name_pairs: &[(String, String)]) -> Option<String> {
    let search_start = floor_char_boundary(text, ref_pos.saturating_sub(50));
    let search_area = &text[search_start..ref_pos];

    let mut best: Option<&str> = None;
    let mut best_end: usize = 0;

    for (alias, canonical) in name_pairs {
        for (np, _) in search_area.match_indices(alias.as_str()) {
            let end = search_start + np + alias.len();
            let gap = ref_pos - end;
            if gap <= 6 && end > best_end {
                best_end = end;
                best = Some(canonical);
            }
        }
    }

    best.map(|s| s.to_string())
}

/// Find the explicit addressee from `谓X曰/道` or `对X曰/道` patterns.
fn find_dialog_directed(text: &str, pos: usize, name_pairs: &[(String, String)]) -> Option<String> {
    let before = &text[..pos];

    // 谓X曰/谓X道 → X is the addressee
    if let Some(wei_pos) = before.rfind("谓") {
        let between = &text[wei_pos + 3..pos];
        if let Some(name) = longest_match(between, name_pairs) {
            return Some(name);
        }
    }

    // 对X曰/对X道 → X is the addressee (only if X is between 对 and marker)
    if let Some(dui_pos) = before.rfind("对") {
        let between = &text[dui_pos + 3..pos];
        // Only if there's actually a name between 对 and the marker
        if let Some(name) = longest_match(between, name_pairs) {
            return Some(name);
        }
    }

    None
}

/// Find the longest matching alias in `text` and return its canonical name.
fn longest_match<'a>(text: &str, name_pairs: &'a [(String, String)]) -> Option<String> {
    let mut best: Option<&str> = None;
    let mut best_len: usize = 0;

    for (alias, canonical) in name_pairs {
        if text.contains(alias.as_str()) && alias.len() > best_len {
            best_len = alias.len();
            best = Some(canonical);
        }
    }

    best.map(|s| s.to_string())
}

/// Extract speech content after a dialog marker, up to the next marker or 300 bytes.
fn extract_speech_span<'a>(text: &'a str, start: usize, all_markers: &[usize]) -> &'a str {
    let start = start.min(text.len());
    let end = all_markers
        .iter()
        .find(|&&p| p >= start)
        .map(|&p| p)
        .unwrap_or((start + 300).min(text.len()));
    let end = floor_char_boundary(text, end);
    &text[start..end]
}

/// Rules for detecting relation types from keywords.
///
/// Each rule pairs a list of trigger keywords with a relation type. A keyword
/// only fires when it appears within [`RELATION_KEYWORD_PROXIMITY`] bytes of
/// BOTH character names in the same chapter — so common words like "兄弟" or
/// "主公" do not cause false positives on their own; they must co-occur with
/// both names in a tight window.
pub const RELATION_TYPE_RULES: &[(&[&str], &str)] = &[
    (&["夫妻", "夫妇", "配为夫妇", "嫁与", "娶"], "夫妻"),
    (&["结义", "兄弟", "义兄", "义妹", "拜为兄弟"], "结义"),
    (&["父子", "父女"], "父子"),
    (&["母子", "母女"], "母女"),
    (&["师徒", "师父", "徒弟", "拜师"], "师徒"),
    // "君臣" as a noun may appear in narration; "主公" is handled by dialog
    // chain extraction (see DIALOG_ADDRESS_RULES / extract_dialog_relations).
    (&["君臣", "丞相", "陛下"], "君臣"),
    (&["挚友", "好友"], "挚友"),
    (&["仇敌", "仇人", "对头"], "仇敌"),
    (&["姐妹", "姊妹"], "姐妹"),
    (&["亲戚", "亲属", "亲家"], "亲戚"),
];

/// Collect all names and aliases for a given canonical name (across all novels).
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

/// Maximum byte distance between a keyword and a character name for the
/// relation to be attributed to that keyword. 40 bytes ≈ 13 Chinese chars,
/// enough to cover a short clause but not cross sentence boundaries.
const RELATION_KEYWORD_PROXIMITY: usize = 40;

/// Detect relation type between character `a` and `b` from context text.
///
/// Returns the specific relation type only when a keyword appears within
/// [`RELATION_KEYWORD_PROXIMITY`] bytes of any occurrence of BOTH character
/// names. Falls back to "关联" (generic relation).
pub fn detect_relation_type(context: &str, a: &str, b: &str) -> String {
    if context.is_empty() {
        return "关联".to_string();
    }

    let a_names = all_names_for(a);
    let b_names = all_names_for(b);

    for &(keywords, rtype) in RELATION_TYPE_RULES {
        for kw in keywords {
            if !context.contains(kw) {
                continue;
            }
            // Check every occurrence of the keyword (a keyword may appear
            // multiple times in the context).
            for (kw_pos, _) in context.match_indices(kw) {
                // A name is "near" if ANY occurrence of ANY alias is within
                // RELATION_KEYWORD_PROXIMITY bytes of this keyword occurrence.
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
                    return rtype.to_string();
                }
            }
        }
    }
    "关联".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_fuqi_relation() {
        // Both 宋江 and 扈三娘 belong to 水浒传; "配为夫妇" near both names.
        let ctx = "宋江与扈三娘配为夫妇，众人皆贺。";
        let rtype = detect_relation_type(ctx, "宋江", "扈三娘");
        assert_eq!(rtype, "夫妻", "配为夫妇 near both names should detect 夫妻");
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
        // 孔明 says "主公" in reply to 刘备 → should detect 君臣
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
            "孔明曰主公对刘备 should be 君臣, got {relations:?}"
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
        // "诗曰" should be skipped, so only 玄德's dialog remains (无 keyword → no relation)
        assert!(
            relations.is_empty(),
            "诗曰 should be skipped, got {relations:?}"
        );
    }

    #[test]
    fn dialog_works_with_dao_marker() {
        // 水浒传-style: 宋江道： 哥哥在上
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
            "吴用道哥哥对宋江 should be 结义, got {relations:?}"
        );
    }

    #[test]
    fn dialog_resolves_wei_x_yue() {
        // 玄德谓孔明曰: 玄德 is speaking TO 孔明
        let text = "玄德谓孔明曰：先生何以教我？";
        let name_pairs = vec![
            ("孔明".to_string(), "诸葛亮".to_string()),
            ("玄德".to_string(), "刘备".to_string()),
            ("诸葛亮".to_string(), "诸葛亮".to_string()),
            ("刘备".to_string(), "刘备".to_string()),
        ];
        let relations = extract_dialog_relations(text, &name_pairs);
        // 玄德 is speaking, no address keyword → empty
        assert!(relations.is_empty());
    }

    #[test]
    fn relation_rules_have_no_empty_keywords() {
        for &(keywords, rtype) in RELATION_TYPE_RULES {
            assert!(!keywords.is_empty(), "rule for {rtype} has no keywords");
            for kw in keywords {
                assert!(!kw.is_empty(), "empty keyword in rule {rtype}");
            }
        }
    }
}
