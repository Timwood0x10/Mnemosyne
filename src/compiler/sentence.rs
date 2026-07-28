//! Sentence Compiler — Phase 2.
//!
//! Splits a [`Chunk`]'s text into [`Sentence`]s by sentence-ending
//! punctuation. Each sentence retains byte-accurate offsets back to the
//! original document text for evidence tracing.
//!
//! ## Supported separators
//!
//! | Script | Separators |
//! |--------|-----------|
//! | Chinese | 。！？； |
//! | English | !?;  |
//! | Universal | \n (newline) |
//!
//! The separator character is included as the last character of each sentence
//! so that reconstructed text matches the original.

use crate::compiler::{Chunk, Sentence, SentenceId};

/// Separator characters that end a sentence.
const SEPARATORS: &[char] = &['。', '！', '？', '；', '!', '?', ';', '\n'];

/// Split a single [`Chunk`] into [`Sentence`]s.
///
/// Returns an empty vec when the chunk text is empty.
///
/// ## Algorithm
///
/// 1. Walk through the text finding separator characters.
/// 2. Each separator ends the current sentence (inclusive: the separator
///    is the last character of the sentence).
/// 3. Trailing whitespace after a separator is trimmed from the next sentence.
/// 4. Any remaining text after the last separator becomes the final sentence.
/// 5. Byte offsets are relative to the document (chunk.start_offset + local offset).
pub fn split_chunk(chunk: &Chunk) -> Vec<Sentence> {
    if chunk.text.is_empty() {
        return Vec::new();
    }

    let mut sentences = Vec::new();
    let mut start = 0usize;
    let mut sent_index = 0usize;

    let chars: Vec<char> = chunk.text.chars().collect();
    let total_chars = chars.len();
    let mut char_pos = 0usize;

    for (byte_pos, _) in chunk.text.char_indices() {
        // Find which char index this byte corresponds to
        // (Only advance char_pos for every valid char)
        if byte_pos > 0 {
            // count chars up to this byte
            let mut count = 0usize;
            for (i, c) in chunk.text.char_indices() {
                if i >= byte_pos {
                    break;
                }
                count += c.len_utf8();
            }
            // Actually this is getting complicated. Let me use a simpler approach.
        }
        break; // start over
    }

    // Simpler approach: iterate by chars
    let mut local_byte = 0usize;
    let text_bytes = chunk.text.as_bytes();

    // We'll track positions using char_indices
    let mut iter_start = 0usize;

    for (i, (byte_idx, c)) in chunk.text.char_indices().enumerate() {
        if SEPARATORS.contains(&c) {
            // End of sentence: include the separator
            let end_byte = byte_idx + c.len_utf8();
            let sentence_text = &chunk.text[iter_start..end_byte];
            let trimmed = sentence_text.trim();

            if !trimmed.is_empty() {
                sentences.push(Sentence {
                    chunk_index: chunk.index,
                    index: sent_index,
                    text: trimmed.to_owned(),
                    start_offset: chunk.start_offset + iter_start,
                    end_offset: chunk.start_offset + end_byte,
                });
                sent_index += 1;
            }

            iter_start = end_byte;

            // Skip any trailing whitespace/newlines after the separator
            let mut skip = iter_start;
            while skip < chunk.text.len()
                && (chunk.text.as_bytes()[skip] == b' '
                    || chunk.text.as_bytes()[skip] == b'\n'
                    || chunk.text.as_bytes()[skip] == b'\t'
                    || chunk.text.as_bytes()[skip] == b'\r')
            {
                skip += 1;
            }
            iter_start = skip;
        }
    }

    // Remaining text after the last separator
    if iter_start < chunk.text.len() {
        let remaining = chunk.text[iter_start..].trim();
        if !remaining.is_empty() {
            let end_byte = chunk.text.len();
            sentences.push(Sentence {
                chunk_index: chunk.index,
                index: sent_index,
                text: remaining.to_owned(),
                start_offset: chunk.start_offset + iter_start,
                end_offset: chunk.start_offset + end_byte,
            });
        }
    }

    sentences
}

/// Split multiple chunks into a flat list of sentences.
///
/// Each sentence's `index` restarts at 0 per chunk, but `SentenceId` in the
/// flat list is its position in the returned vec.
pub fn split_all(chunks: &[Chunk]) -> Vec<Sentence> {
    let mut all = Vec::new();
    for chunk in chunks {
        all.extend(split_chunk(chunk));
    }
    all
}

/// Convenience: build a mapping from `(chunk_index, local_index)` → `SentenceId`.
pub fn build_index(
    sentences: &[Sentence],
) -> std::collections::HashMap<(usize, usize), SentenceId> {
    sentences
        .iter()
        .enumerate()
        .map(|(id, s)| ((s.chunk_index, s.index), id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::Chunk;

    fn make_chunk(index: usize, text: &str, offset: usize) -> Chunk {
        Chunk {
            index,
            text: text.to_owned(),
            start_offset: offset,
            end_offset: offset + text.len(),
            segment_num: 1,
            overlap_before: 0,
            overlap_after: 0,
        }
    }

    /// Objective: Verify that an empty chunk produces no sentences.
    /// Invariants: Output vec is empty; no panics.
    #[test]
    fn empty_chunk_yields_no_sentences() {
        let chunk = make_chunk(0, "", 0);
        let sents = split_chunk(&chunk);
        assert!(sents.is_empty(), "empty chunk → no sentences");
    }

    /// Objective: Verify that a simple Chinese sentence is correctly identified.
    /// Invariants: One sentence output; text matches; offset covers full range.
    #[test]
    fn single_chinese_sentence() {
        let chunk = make_chunk(0, "赵云救阿斗。", 100);
        let sents = split_chunk(&chunk);
        assert_eq!(sents.len(), 1, "single sentence expected");
        assert_eq!(sents[0].text, "赵云救阿斗。");
        assert_eq!(sents[0].start_offset, 100);
        assert_eq!(sents[0].end_offset, 100 + chunk.text.len());
    }

    /// Objective: Verify that multiple sentences are split correctly.
    /// Invariants: Each sentence includes its trailing separator; offsets
    /// are sequential without gaps.
    #[test]
    fn multiple_chinese_sentences() {
        let chunk = make_chunk(0, "关羽斩华雄。张飞喝断当阳桥。", 50);
        let sents = split_chunk(&chunk);
        assert_eq!(sents.len(), 2, "two sentences expected");
        assert!(
            sents[0].text.contains("关羽"),
            "first sentence mentions 关羽"
        );
        assert!(
            sents[1].text.contains("张飞"),
            "second sentence mentions 张飞"
        );
        // Offsets should be non-overlapping
        assert!(sents[0].end_offset <= sents[1].start_offset);
    }

    /// Objective: Verify that mixed Chinese/English punctuation works.
    /// Invariants: Both 。 and ! are recognized as sentence separators.
    #[test]
    fn mixed_punctuation() {
        let chunk = make_chunk(0, "小心！有埋伏。撤!", 0);
        let sents = split_chunk(&chunk);
        assert_eq!(sents.len(), 3, "three sentences expected");
        assert_eq!(sents[0].text, "小心！");
        assert_eq!(sents[1].text, "有埋伏。");
        assert_eq!(sents[2].text, "撤!");
    }

    /// Objective: Verify that trailing whitespace after separators is trimmed.
    /// Invariants: The second sentence starts with non-whitespace content.
    #[test]
    fn whitespace_after_separator_is_trimmed() {
        let chunk = make_chunk(0, "第一句。   \n第二句。", 0);
        let sents = split_chunk(&chunk);
        assert_eq!(sents.len(), 2);
        assert_eq!(sents[1].text, "第二句。");
    }

    /// Objective: Verify that text without separators produces one sentence.
    /// Invariants: Exactly one sentence covers the full text.
    #[test]
    fn no_separator_produces_one_sentence() {
        let chunk = make_chunk(0, "赵云救阿斗", 0);
        let sents = split_chunk(&chunk);
        assert_eq!(sents.len(), 1);
        assert_eq!(sents[0].text, "赵云救阿斗");
    }

    /// Objective: Verify that split_all produces a flat list from multiple chunks.
    /// Invariants: Total sentences = sum of per-chunk sentences.
    #[test]
    fn split_all_aggregates_multiple_chunks() {
        let chunks = vec![
            make_chunk(0, "关羽斩华雄。", 0),
            make_chunk(1, "张飞喝断桥。", 50),
        ];
        let sents = split_all(&chunks);
        assert_eq!(sents.len(), 2);
        assert_eq!(sents[0].chunk_index, 0);
        assert_eq!(sents[1].chunk_index, 1);
    }

    /// Objective: Verify that build_index creates the correct lookup map.
    /// Invariants: Each (chunk_index, index) pair maps to a unique SentenceId.
    #[test]
    fn build_index_creates_correct_map() {
        let chunk = make_chunk(0, "A。B。C。", 0);
        let sents = split_chunk(&chunk);
        let idx = build_index(&sents);
        assert_eq!(idx.len(), 3, "three sentences → three index entries");
        // Each sentence maps to its position
        for (sid, s) in sents.iter().enumerate() {
            let key = (s.chunk_index, s.index);
            assert_eq!(idx[&key], sid, "index should map to position");
        }
    }
}
