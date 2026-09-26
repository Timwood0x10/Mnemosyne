/// Corpus loading, chapter splitting, and Chinese numeral parsing.
use std::path::Path;

/// A single chapter from a novel.
#[derive(Debug, Clone)]
pub struct Chapter {
    pub num: i32,
    pub text: String,
    /// Byte range of `text` inside the source handed to
    /// [`split_into_chapters`]: `source[start_offset .. end_offset] == text`
    /// exactly, so a chapter-local offset plus `start_offset` maps into the
    /// full-source coordinate space (the invariant evidence rows rely on to
    /// stay re-locatable). Coordinates are into the source AS PASSED — a
    /// leading BOM, when present, stays inside the coordinate space instead
    /// of shifting every range by 3 bytes.
    pub start_offset: usize,
    pub end_offset: usize,
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
///
/// The old implementation accumulated 十/百/千 directly into `total`, then
/// when it met 万 it only multiplied the residual `temp` — so 十万 parsed as
/// 10*10 + 1*10000 = 10010 instead of 100000, and 一百二十万 as 10120
/// instead of 1200000. The correct model is segmented: digits and 十/百/千
/// build the current section; 万 (and 亿 if ever added) multiplies the whole
/// section into the running total and resets it.
pub fn chinese_to_int(s: &str) -> i32 {
    let s = s.trim();
    let mut total = 0i32; // 万-accumulated value (segments already scaled)
    let mut section = 0i32; // current sub-10000 segment
    let mut temp = 0i32; // pending digit
    for c in s.chars() {
        if let Some(n) = cn_num_value(c) {
            if n >= 10_000 {
                // 万/亿: scale the whole section and fold into total.
                let base = if section == 0 { 1 } else { section };
                total = total.saturating_add(base.saturating_mul(n));
                section = 0;
                temp = 0;
            } else if n >= 10 {
                // 十/百/千: multiply the pending digit into the section.
                let digit = if temp == 0 { 1 } else { temp };
                section = section.saturating_add(digit.saturating_mul(n));
                temp = 0;
            } else {
                temp = n;
            }
        } else if c.is_ascii_digit() {
            temp = temp
                .saturating_mul(10)
                .saturating_add(c as i32 - '0' as i32);
        }
    }
    total.saturating_add(section).saturating_add(temp)
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

        // Content starts after "回" (3-byte char). Record the trimmed
        // body's EXACT range in the source: a heading-to-heading range would
        // not slice back to `content`, and a cumulative length cursor drifts
        // because `trim()` drops the whitespace between chapters.
        let raw = &text[end + '回'.len_utf8()..next_start];
        let leading = raw.len() - raw.trim_start().len();
        let content = raw.trim();
        let content_start = end + '回'.len_utf8() + leading;
        chapters.push(Chapter {
            num: ch_num,
            text: content.to_string(),
            start_offset: content_start,
            end_offset: content_start + content.len(),
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

    /// Objective: Verify 万-scale numerals parse correctly — the old
    /// algorithm folded 十/百/千 into `total` and only scaled the residual
    /// `temp` by 万, so 十万 → 10010 and 一百二十万 → 10120 (audit finding).
    /// Invariants: 十万 == 100000; 一百二十万 == 1200000; 一万 == 10000;
    /// 十万八千 == 108000; 三千五百 == 3500 still holds.
    #[test]
    fn chinese_to_int_wan_scale() {
        assert_eq!(chinese_to_int("一万"), 10000, "一万 must be 10000");
        assert_eq!(chinese_to_int("十万"), 100000, "十万 must be 100000");
        assert_eq!(
            chinese_to_int("一百二十万"),
            1_200_000,
            "一百二十万 must be 1200000"
        );
        assert_eq!(
            chinese_to_int("十万八千"),
            108_000,
            "十万八千 must be 108000"
        );
        // Existing small-number behavior is unchanged.
        assert_eq!(chinese_to_int("三千五百"), 3500);
    }

    /// Objective: Verify an absurdly long Arabic digit string does not panic
    /// (debug builds overflow on plain `temp * 10`). Saturating arithmetic
    /// must clamp instead of panicking.
    /// Invariants: no panic; result is finite (i32::MAX clamped).
    #[test]
    fn chinese_to_int_long_arabic_does_not_panic() {
        let huge = "12345678901234567890";
        let n = chinese_to_int(huge);
        assert!(
            n == i32::MAX || n > 0,
            "long digit string must saturate without panicking, got {n}"
        );
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

    /// Objective: Verify each chapter carries its EXACT source byte range —
    /// `source[start_offset..end_offset] == text` (content-exact after trim,
    /// never a heading-to-heading or cumulative-length approximation).
    /// Invariants: slice equality per chapter; ranges strictly ordered and
    /// non-overlapping; leading in-chapter whitespace excluded.
    #[test]
    fn split_chapters_carries_source_byte_ranges() {
        let text = "序言。\n第一回 开篇\n  正文一。\n第二回 发展\n正文二。\n";
        let chapters = split_into_chapters(text);
        assert_eq!(chapters.len(), 2, "two chapters");
        for ch in &chapters {
            assert_eq!(
                &text[ch.start_offset..ch.end_offset],
                ch.text,
                "chapter {} range must slice the source exactly",
                ch.num
            );
        }
        assert!(
            chapters[0].start_offset < chapters[1].start_offset,
            "ranges follow source order"
        );
        assert!(
            chapters[0].end_offset <= chapters[1].start_offset,
            "ranges must not overlap (the next heading separates them)"
        );
        assert_eq!(
            chapters[0].text, "开篇\n  正文一。",
            "content runs from just after the heading marker, leading blank trimmed"
        );
        assert!(
            !chapters[0].text.starts_with(char::is_whitespace),
            "range starts at the first non-blank byte"
        );
    }

    /// Objective: Verify a leading BOM does NOT shift the byte ranges —
    /// coordinates are into the source as passed, so `raw[start..end] == text`
    /// even when the file starts with U+FEFF (the previous BOM strip made
    /// every range 3 bytes short of the raw file).
    /// Invariants: slice equality against the RAW input; range starts after
    /// the BOM; content excludes the BOM.
    #[test]
    fn split_chapters_bom_keeps_raw_source_coordinates() {
        let raw = "\u{feff}第一回 开篇\n正文一。\n第二回 发展\n正文二。\n";
        let chapters = split_into_chapters(raw);
        assert_eq!(chapters.len(), 2, "BOM must not break marker detection");
        for ch in &chapters {
            assert_eq!(
                &raw[ch.start_offset..ch.end_offset],
                ch.text,
                "chapter {} range must slice the RAW source (BOM included)",
                ch.num
            );
        }
        assert!(
            chapters[0].start_offset >= '\u{feff}'.len_utf8(),
            "range starts past the BOM, got {}",
            chapters[0].start_offset
        );
        assert!(
            !chapters[0].text.starts_with('\u{feff}'),
            "content never contains the BOM"
        );
    }

    #[test]
    fn load_novel_unknown_returns_error() {
        let result = load_novel("unknown", Path::new("corpus"));
        assert!(result.is_err());
    }
}
