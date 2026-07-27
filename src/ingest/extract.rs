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
