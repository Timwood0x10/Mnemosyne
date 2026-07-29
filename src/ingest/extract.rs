//! Text extraction utilities: action sentences, descriptions, death detection.

/// Strong verbs that indicate meaningful events.
pub const STRONG_VERBS: &[&str] = &[
    "杀", "斩", "擒", "捉", "打", "战", "斗", "败", "胜", "攻", "破", "救", "逃", "死", "亡", "卒",
    "殉", "绑", "缚", "捉拿", "骂", "哭", "笑", "怒", "拜", "嫁", "娶", "配", "封", "赐", "赏",
];

/// Keywords indicating clothing descriptions.
pub const CLOTHING_KW: &[&str] = &[
    "头戴", "身穿", "披", "冠", "袍", "铠", "甲", "盔", "胄", "靴", "锦袍", "战袍",
];

/// Keywords indicating personality or appearance descriptions.
pub const PERSONALITY_KW: &[&str] = &[
    "面如", "相貌", "形容", "身材", "性格", "勇猛", "刚烈", "温柔", "英雄", "豪杰", "仗义", "仁德",
    "聪明", "奸诈",
];

/// Keywords indicating death.
pub const DEATH_KW: &[&str] = &[
    "死", "亡", "卒", "杀", "斩首", "身亡", "战死", "阵亡", "去世", "薨",
];

/// Largest char boundary at or before `pos`, clamped to `text.len()`.
///
/// Manual implementation for the project's MSRV (1.85) — `str::floor_char_boundary`
/// is only stable since 1.91. Used wherever we slice by a position that may land
/// inside a multi-byte Chinese character.
pub(crate) fn floor_char_boundary(text: &str, pos: usize) -> usize {
    let mut p = pos.min(text.len());
    while p > 0 && !text.is_char_boundary(p) {
        p -= 1;
    }
    p
}

/// Dialog verbs that signal a speaker attribution: `X[verb]` means "X said".
///
/// Single-character shortnames (e.g. 飞 for 张飞) are only recognized as a
/// character mention when immediately followed by one of these verbs, which
/// disambiguates the shortname from homographs inside multi-character names
/// or common nouns.
pub const DIALOG_VERBS: &[&str] = &[
    "曰", "道", "言", "答", "问", "笑", "怒", "喝", "唤", "叫", "叹", "惊", "喜",
];

/// Action verbs that signal a character acting as a subject: `X[verb]` means
/// "X (performed action)". Used together with [`DIALOG_VERBS`] to broaden
/// single-char recognition beyond pure dialog.
pub const ACTION_VERBS: &[&str] = &[
    "大怒", "大喜", "领兵", "引军", "挺枪", "纵马", "大呼", "拍马", "拔剑", "挺刀", "引兵", "出马",
    "上前", "奋然", "勃然", "大惊", "大败", "引军",
];

/// Find safe single-character shortname matches in `text`.
///
/// Classical Chinese novels frequently abbreviate a 2-character name to its
/// final character in dialog contexts: "飞曰" (张飞 said), "瑜怒" (周瑜 got angry),
/// "云大喜" (赵云 was overjoyed). Naively matching "云" everywhere would cause
/// catastrophic false positives — "云" appears 443 times inside "云长" (关羽's
/// courtesy name) and many more times as the noun "cloud".
///
/// This function only yields a match when ALL of the following hold:
///   1. The single char is **preceded by** punctuation, whitespace, or string
///      start — never by another Chinese character. This excludes "云长"
///      (preceded by nothing, but followed by 长) and "玄德云" (云 is the verb
///      "to say" here, not 赵云).
///   2. The single char is **followed by** a dialog verb (曰/道/言/...) or an
///      action verb (大怒/领兵/...). This excludes bare "云" as a noun.
///
/// Returns `(start, end, char_name)` byte ranges for each safe match.
pub fn find_single_char_matches(
    text: &str,
    cdefs: &[crate::ingest::characters::CharacterDef],
) -> Vec<(usize, usize, String)> {
    let mut out: Vec<(usize, usize, String)> = Vec::new();

    for cdef in cdefs {
        let Some(short) = cdef.single_char else {
            continue;
        };
        let short_len = short.len();

        // Find every occurrence of the short char.
        for (pos, _) in text.match_indices(short) {
            // (1) Preceded by punctuation, whitespace, or string start.
            // Equivalent to "not preceded by a Chinese char".
            let prev_ok = if pos == 0 {
                true
            } else {
                let prev = &text[..pos];
                // Floor to the previous char boundary (multi-byte safe).
                let boundary = floor_char_boundary(prev, prev.len());
                let last_char = prev[boundary..].chars().next();
                match last_char {
                    None => true,
                    Some(c) => {
                        // Treat any CJK Unified Ideograph (U+4E00..=U+9FFF)
                        // as a "Chinese char" that must NOT precede the short.
                        // Punctuation, ASCII whitespace, and fullwidth Latin
                        // all pass through.
                        !('\u{4E00}'..='\u{9FFF}').contains(&c)
                    }
                }
            };
            if !prev_ok {
                continue;
            }

            // (2) Followed by a dialog or action verb.
            let after = &text[pos + short_len..];
            let followed = DIALOG_VERBS.iter().any(|v| after.starts_with(v))
                || ACTION_VERBS.iter().any(|v| after.starts_with(v));
            if !followed {
                continue;
            }

            out.push((pos, pos + short_len, cdef.name.to_string()));
        }
    }

    out
}

/// Find the sentence boundary before position `pos` in `text`.
///
/// Returns the byte position where the sentence containing `pos` begins —
/// i.e. right after the previous sentence-ending separator, or the start of
/// the search range if none is found. Positions are floored to valid char
/// boundaries so callers may pass a position inside a multi-byte character.
fn sentence_start(text: &str, pos: usize) -> usize {
    // Floor both positions to valid UTF-8 char boundaries
    let pos = floor_char_boundary(text, pos);
    let search_start = floor_char_boundary(text, pos.saturating_sub(200));
    for sep in &['。', '！', '？', '!', '?', '\n', '；'] {
        if let Some(p) = text[search_start..pos].rfind(*sep) {
            // Position right AFTER the separator: the sentence begins here,
            // so the extracted span excludes leading punctuation.
            return search_start + p + sep.len_utf8();
        }
    }
    search_start
}

/// Find the sentence boundary after position `pos` in `text`.
///
/// Returns the byte position of the next sentence separator, or the
/// end of the search range if none is found. Positions are floored to
/// valid char boundaries to avoid panicking on multi-byte Chinese text.
fn sentence_end(text: &str, pos: usize) -> usize {
    // Floor both positions to valid UTF-8 char boundaries
    let pos = floor_char_boundary(text, pos);
    let search_end = floor_char_boundary(text, std::cmp::min(text.len(), pos + 200));
    for sep in &['。', '！', '？', '!', '?', '\n', '；'] {
        if let Some(p) = text[pos..search_end].find(*sep) {
            // Return the separator position (start of the separator char)
            return pos + p;
        }
    }
    search_end
}

/// Extract the sentence containing `name` that also contains a strong verb.
///
/// Returns `None` if no strong verb is found, or the sentence is too long.
pub fn extract_action_sentence(text: &str, name: &str) -> Option<String> {
    for (start, _) in text.match_indices(name) {
        let sent_start = sentence_start(text, start);
        let sent_end = sentence_end(text, start + name.len());
        let sent = text[sent_start..sent_end].trim();
        if sent.is_empty() || sent.len() > 300 {
            continue;
        }
        for v in STRONG_VERBS {
            if sent.contains(v) {
                let truncated: String = sent.chars().take(200).collect();
                return Some(truncated);
            }
        }
    }
    None
}

/// Extract clothing and personality descriptions from context around `name`.
pub fn extract_description(text: &str, name: &str) -> (String, String) {
    let mut clothing_parts: Vec<String> = Vec::new();
    let mut personality_parts: Vec<String> = Vec::new();

    for (start, _) in text.match_indices(name) {
        let sent_end = sentence_end(text, start + name.len());
        let sent = text[start..sent_end].trim();

        if CLOTHING_KW.iter().any(|kw| sent.contains(kw)) {
            let clipped: String = sent.chars().take(150).collect();
            clothing_parts.push(clipped);
        }
        if PERSONALITY_KW.iter().any(|kw| sent.contains(kw)) {
            let clipped: String = sent.chars().take(150).collect();
            personality_parts.push(clipped);
        }
    }

    clothing_parts.truncate(3);
    personality_parts.truncate(3);
    (clothing_parts.join("; "), personality_parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_action_sentence_finds_verb() {
        let text = "武松提起哨棒，照头一下打去，那大虫便死了。";
        let result = extract_action_sentence(text, "武松");
        assert!(result.is_some(), "should find '打' or '死' verb");
        let s = result.unwrap();
        assert!(
            s.contains("打") || s.contains("死"),
            "sentence should contain a verb"
        );
    }

    #[test]
    fn extract_action_sentence_no_verb() {
        let text = "武松是一个好汉。";
        let result = extract_action_sentence(text, "武松");
        assert!(result.is_none());
    }

    #[test]
    fn extract_action_sentence_name_not_found() {
        let text = "这是一个普通的句子。";
        let result = extract_action_sentence(text, "不存在");
        assert!(result.is_none());
    }

    #[test]
    fn extract_description_clothing() {
        let text = "只见那武松头戴一顶新头巾，身穿一领新衣裳。";
        let (clothing, _) = extract_description(text, "武松");
        assert!(!clothing.is_empty(), "should find clothing kw");
        assert!(clothing.contains("头戴") || clothing.contains("身穿"));
    }

    #[test]
    fn extract_description_personality() {
        let text = "那武松相貌堂堂，性格刚烈，是个英雄好汉。";
        let (_, personality) = extract_description(text, "武松");
        assert!(!personality.is_empty(), "should find personality kw");
    }

    #[test]
    fn extract_description_no_match() {
        let text = "武松在路上走了几日。";
        let (clothing, personality) = extract_description(text, "武松");
        assert!(clothing.is_empty());
        assert!(personality.is_empty());
    }

    #[test]
    fn strong_verbs_list_is_populated() {
        assert!(!STRONG_VERBS.is_empty());
        assert!(STRONG_VERBS.contains(&"杀"));
        assert!(STRONG_VERBS.contains(&"死"));
    }

    #[test]
    fn sentence_start_handles_boundary() {
        let text = "甲。乙。丙。";
        // pos=7 lands inside '乙' (bytes 6..9); floor to 6, then the sentence
        // starts right after the previous '。' (bytes 3..6), i.e. byte 6.
        let start = sentence_start(text, 7);
        assert_eq!(
            start, 6,
            "sentence start should follow the previous separator"
        );
    }

    #[test]
    fn sentence_end_handles_boundary() {
        let text = "甲。乙。丙。";
        let end = sentence_end(text, 2);
        assert_eq!(end, 3, "should end at first period");
    }
}
