/// Static character definitions for the four classical Chinese novels.
use std::collections::HashMap;

/// A character definition — canonical name + known aliases.
#[derive(Debug, Clone)]
pub struct CharacterDef {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    /// Optional single-character shortname used in dialog (e.g. 张飞→飞, 周瑜→瑜).
    ///
    /// Classical Chinese novels frequently abbreviate a 2-character name to
    /// its final character in dialog contexts: "飞曰"、"瑜怒"、"云大喜".
    /// These forms are NOT added to `aliases` because bare single-character
    /// matching would cause catastrophic false positives:
    ///   - "云" matches inside "云长" (关羽's courtesy name, 443 occurrences)
    ///   - "云" matches inside "乌云"/"碧云" (clouds, generic noun)
    ///   - "飞" matches inside "飞马"/"飞箭" (flying horse/arrow)
    ///
    /// Instead, the pipeline matches `single_char` only in **safe contexts**:
    /// preceded by punctuation/whitespace/string-start AND followed by a
    /// dialog verb (曰/道/言/笑/怒/...) or action verb (大怒/领兵/挺枪/...).
    /// See [`crate::ingest::extract::find_single_char_matches`].
    pub single_char: Option<&'static str>,
}

mod data;

use data::{HONGLOU, SANGUO, SHUIHU, XIYOU};

/// List of supported novel names in processing order.
pub const NOVELS: &[&str] = &["水浒传", "三国演义", "红楼梦", "西游记"];

/// Map a novel name to its character definitions.
pub fn get_novel_characters(novel: &str) -> &'static [CharacterDef] {
    match novel {
        "水浒传" => SHUIHU,
        "三国演义" => SANGUO,
        "红楼梦" => HONGLOU,
        "西游记" => XIYOU,
        _ => &[],
    }
}

/// Build a per-novel map from any alias (including the canonical name) to the
/// canonical character name.
///
/// Alias resolution is scoped to a single novel because courtesy names collide
/// across novels — e.g. `公明` is both 宋江 (水浒传) and 徐晃 (三国演义). A
/// global map cannot disambiguate these, so the pipeline resolves aliases
/// within each novel's own map.
pub fn build_alias_map_for_novel(novel: &str) -> HashMap<&'static str, &'static str> {
    let mut map = HashMap::new();
    for cdef in get_novel_characters(novel) {
        map.insert(cdef.name, cdef.name);
        for a in cdef.aliases {
            map.insert(*a, cdef.name);
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_novels_have_characters() {
        for novel in NOVELS {
            let chars = get_novel_characters(novel);
            assert!(!chars.is_empty(), "{novel} should have characters");
        }
    }

    #[test]
    fn all_names_are_unique_within_novel() {
        for novel in NOVELS {
            let chars = get_novel_characters(novel);
            let mut names = std::collections::HashSet::new();
            for c in chars {
                assert!(
                    names.insert(c.name),
                    "duplicate name {} in {}",
                    c.name,
                    novel
                );
                for a in c.aliases {
                    assert!(names.insert(a), "duplicate alias {} in {}", a, novel);
                }
            }
        }
    }

    #[test]
    fn alias_map_resolves_all_names() {
        // Alias maps are per-novel: courtesy names like 公明 belong to both
        // 宋江 (水浒传) and 徐晃 (三国演义), so each novel must resolve its own.
        for novel in NOVELS {
            let map = build_alias_map_for_novel(novel);
            for c in get_novel_characters(novel) {
                assert_eq!(
                    map.get(c.name).copied(),
                    Some(c.name),
                    "canonical name {} should map to itself in {novel}",
                    c.name
                );
                for a in c.aliases {
                    assert_eq!(
                        map.get(a).copied(),
                        Some(c.name),
                        "alias {a} should map to {} in {novel}",
                        c.name
                    );
                }
            }
        }
    }
}
