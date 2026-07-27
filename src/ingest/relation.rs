/// Relation type detection from context text.
use super::characters::{NOVELS, get_novel_characters};

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
    // "主公" / "丞相" are role-address terms but in classical novels they
    // are THE canonical 君臣 signal (e.g. 孔明 calls 刘备 "主公"). The
    // dual-name proximity requirement filters incidental mentions.
    (&["君臣", "主公", "丞相", "陛下"], "君臣"),
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
    fn relation_rules_have_no_empty_keywords() {
        for &(keywords, rtype) in RELATION_TYPE_RULES {
            assert!(!keywords.is_empty(), "rule for {rtype} has no keywords");
            for kw in keywords {
                assert!(!kw.is_empty(), "empty keyword in rule {rtype}");
            }
        }
    }
}
