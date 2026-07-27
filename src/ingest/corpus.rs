/// Corpus loading, chapter splitting, and Chinese numeral parsing.
use std::collections::HashMap;
use std::path::Path;

/// A single chapter from a novel.
#[derive(Debug, Clone)]
pub struct Chapter {
    pub num: i32,
    pub text: String,
}

/// Known corpus file names.
pub fn novel_filename(novel: &str) -> Option<&'static str> {
    match novel {
        "水浒传" => Some("水浒传.txt"),
        "三国演义" => Some("三国演义.txt"),
        "红楼梦" => Some("红楼梦.txt"),
        "西游记" => Some("西游记.txt"),
        _ => None,
    }
}

const CN_NUMS: &[(char, i32)] = &[
    ('零', 0),
    ('一', 1),
    ('二', 2),
    ('三', 3),
    ('四', 4),
    ('五', 5),
    ('六', 6),
    ('七', 7),
    ('八', 8),
    ('九', 9),
    ('十', 10),
    ('百', 100),
    ('千', 1000),
    ('万', 10000),
];

fn cn_num_value(c: char) -> Option<i32> {
    CN_NUMS.iter().find(|&&(ch, _)| ch == c).map(|&(_, v)| v)
}

/// Parse Chinese numeral string to integer.
///
/// Handles both pure Chinese numerals ("一百二十") and mixed
/// Arabic-and-Chinese ("第120回").
pub fn chinese_to_int(s: &str) -> i32 {
    let s = s.trim();
    let mut total = 0i32;
    let mut temp = 0i32;
    for c in s.chars() {
        if let Some(n) = cn_num_value(c) {
            if n >= 10 {
                if temp == 0 {
                    temp = 1;
                }
                total += temp * n;
                temp = 0;
            } else {
                temp = n;
            }
        } else if c.is_ascii_digit() {
            temp = temp * 10 + (c as i32 - '0' as i32);
        }
    }
    total + temp
}

/// Check if `s` is a valid chapter numeral: non-empty and containing only
/// Chinese numerals (零一二三四五六七八九十百) and/or Arabic digits.
///
/// This prevents false matches where "第" in body text (e.g. "第三日")
/// is followed by a distant "回" from the actual chapter marker.
fn is_valid_chapter_numeral(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| cn_num_value(c).is_some() || c.is_ascii_digit())
}

/// Splits the full text of a novel into chapters.
///
/// Looks for patterns like "第一回", "第一百二十回" etc. Only accepts
/// markers where the text between "第" and "回" consists solely of numerals.
pub fn split_into_chapters(text: &str) -> Vec<Chapter> {
    let text = text.trim_start_matches('\u{feff}');
    let mut chapters = Vec::new();

    let mut pos = 0;

    while let Some(i) = text[pos..].find('第') {
        // Find "第"
        let start = pos + i;

        // Find "回" after "第"
        let end = match text[start + 3..].find('回') {
            Some(i) => start + 3 + i,
            None => break,
        };

        // Extract the numeral between 第 and 回
        let num_str = &text[start + 3..end];
        // Validate: must be non-empty and contain only numerals.
        // This rejects false matches like "第三日...回" where "第"
        // appears in body text and "回" is from a distant chapter marker.
        if !is_valid_chapter_numeral(num_str) {
            pos = start + '第'.len_utf8();
            continue;
        }
        let ch_num = chinese_to_int(num_str);
        if ch_num == 0 {
            // Skip past "第" (3-byte char) to avoid re-matching the same position
            pos = start + '第'.len_utf8();
            continue;
        }

        // Find end of this chapter: next "第...回" or end of text
        let next_start = {
            // Skip past "回" (3-byte char), not just +1 byte
            let search_from = end + '回'.len_utf8();
            if search_from >= text.len() {
                text.len()
            } else {
                let mut next_pos = text.len();
                let mut search_pos = search_from;
                while let Some(i) = text[search_pos..].find('第') {
                    let n = search_pos + i;
                    let n_end = match text[n + 3..].find('回') {
                        Some(i) => n + 3 + i,
                        None => break,
                    };
                    let n_num_str = &text[n + 3..n_end];
                    // Validate the candidate numeral before accepting it
                    if !is_valid_chapter_numeral(n_num_str) {
                        search_pos = n + '第'.len_utf8();
                        if search_pos >= text.len() {
                            break;
                        }
                        continue;
                    }
                    let n_ch_num = chinese_to_int(n_num_str);
                    // Accept the first valid marker whose chapter number is
                    // greater than the current chapter number.
                    if n_ch_num > ch_num {
                        next_pos = n;
                        break;
                    }
                    search_pos = n + '第'.len_utf8();
                    if search_pos >= text.len() {
                        break;
                    }
                }
                next_pos
            }
        };

        // Content starts after "回" (3-byte char)
        let content = text[end + '回'.len_utf8()..next_start].trim().to_string();
        chapters.push(Chapter {
            num: ch_num,
            text: content,
        });

        pos = next_start;
        if pos >= text.len() {
            break;
        }
    }

    chapters.sort_by_key(|c| c.num);
    chapters
}

/// Load and split a novel's text from the corpus directory.
pub fn load_novel(novel: &str, corpus_dir: &Path) -> std::io::Result<Vec<Chapter>> {
    let fname = novel_filename(novel).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("unknown novel: {novel}"),
        )
    })?;
    let path = corpus_dir.join(fname);
    let text = std::fs::read_to_string(&path)?;
    Ok(split_into_chapters(&text))
}

/// Return the mapping from novel name to its file path
pub fn novel_file_map() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    m.insert("水浒传", "水浒传.txt");
    m.insert("三国演义", "三国演义.txt");
    m.insert("红楼梦", "红楼梦.txt");
    m.insert("西游记", "西游记.txt");
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_to_int_basic() {
        assert_eq!(chinese_to_int("一"), 1);
        assert_eq!(chinese_to_int("十"), 10);
        assert_eq!(chinese_to_int("一百"), 100);
        assert_eq!(chinese_to_int("一百二十"), 120);
    }

    #[test]
    fn chinese_to_int_arabic_mixed() {
        assert_eq!(chinese_to_int("120"), 120);
    }

    #[test]
    fn chinese_to_int_complex() {
        assert_eq!(chinese_to_int("三千五百"), 3500);
    }

    #[test]
    fn chinese_to_int_zero_edge() {
        assert_eq!(chinese_to_int("零"), 0);
        assert_eq!(chinese_to_int(""), 0);
    }

    #[test]
    fn chinese_to_int_eleven() {
        assert_eq!(chinese_to_int("十一"), 11);
        assert_eq!(chinese_to_int("二十一"), 21);
    }

    #[test]
    fn split_chapters_finds_none_in_empty() {
        let chapters = split_into_chapters("");
        assert!(chapters.is_empty());
    }

    #[test]
    fn split_chapters_simple() {
        let text = "第一回 开篇\n这是正文内容。\n第二回 发展\n更多内容。\n";
        let chapters = split_into_chapters(text);
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].num, 1);
        assert_eq!(chapters[1].num, 2);
    }

    #[test]
    fn load_novel_unknown_returns_error() {
        let result = load_novel("unknown", Path::new("corpus"));
        assert!(result.is_err());
    }
}
