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
const SEPARATORS: &[char] = &['。', '！', '？', '；', '.', '!', '?', ';', '\n'];

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
    let mut sent_index = 0usize;
    let mut iter_start = 0usize;

    for (byte_idx, c) in chunk.text.char_indices() {
        if SEPARATORS.contains(&c) {
            // End of sentence: include the separator character
            let end_byte = byte_idx + c.len_utf8();
            // Guard: iter_start may have been advanced past this position
            // by whitespace-skipping after the previous separator.
            let slice_start = if iter_start > byte_idx {
                byte_idx
            } else {
                iter_start
            };
            if slice_start >= chunk.text.len() {
                break;
            }
            let sentence_text = &chunk.text[slice_start..end_byte];
            let trimmed = sentence_text.trim();

            if !trimmed.is_empty() {
                // Adjust offsets to match the trimmed text: the original
                // iter_start/end_byte include leading/trailing whitespace,
                // but `text` is trimmed. Without this adjustment,
                // `document[start_offset..end_offset] != text`, breaking
                // document-level offset math for evidence tracing (NEW-H26).
                let leading = sentence_text.len() - sentence_text.trim_start().len();
                let trailing = sentence_text.len() - sentence_text.trim_end().len();
                sentences.push(Sentence {
                    chunk_index: chunk.index,
                    index: sent_index,
                    text: trimmed.to_owned(),
                    start_offset: chunk.start_offset + slice_start + leading,
                    end_offset: chunk.start_offset + end_byte - trailing,
                });
                sent_index += 1;
            }

            iter_start = end_byte;

            // Skip trailing whitespace/newlines after the separator
            while iter_start < chunk.text.len()
                && matches!(
                    chunk.text.as_bytes()[iter_start],
                    b' ' | b'\n' | b'\t' | b'\r'
                )
            {
                iter_start += 1;
            }
            // Guard: whitespace skip may have consumed the rest of the text
            if iter_start >= chunk.text.len() {
                break;
            }
        }
    }

    // Remaining text after the last separator (or the whole chunk when it
    // has no separators). Apply the SAME leading/trailing-whitespace offset
    // adjustment as the separator branch: without it
    // `document[start_offset..end_offset] != text` for every tail sentence,
    // shifting mention offsets computed by `scan_sentences`.
    if iter_start < chunk.text.len() {
        let tail = &chunk.text[iter_start..];
        let remaining = tail.trim();
        if !remaining.is_empty() {
            let leading = tail.len() - tail.trim_start().len();
            let trailing = tail.len() - tail.trim_end().len();
            sentences.push(Sentence {
                chunk_index: chunk.index,
                index: sent_index,
                text: remaining.to_owned(),
                start_offset: chunk.start_offset + iter_start + leading,
                end_offset: chunk.start_offset + chunk.text.len() - trailing,
            });
        }
    }

    sentences
}

/// Split multiple chunks into a flat list of sentences.
///
/// Each sentence's `index` restarts at 0 per chunk, but [`SentenceId`] in the
/// flat list is its position in the returned vec.
///
/// **Deduplication:** When chunks overlap (the default `chunk::Config` has
/// `overlap = 200`), sentences in the overlap region appear in BOTH chunks.
/// Without dedup, `extract::compile` would process them twice, creating
/// duplicate events and relations. Dedup by `start_offset` alone: a sentence
/// that crosses a chunk boundary appears as a *truncated tail* in chunk N
/// (same start, shorter end) and as the full sentence in chunk N+1 — two
/// distinct `(start, end)` keys, both kept under the old pair-keyed set, so
/// boundary-crossing sentences were still processed twice (NEW-C21).
pub fn split_all(chunks: &[Chunk]) -> Vec<Sentence> {
    let mut all: Vec<Sentence> = Vec::new();
    for chunk in chunks {
        for mut sent in split_chunk(chunk) {
            // Containment dedup: a sentence that starts BEFORE the overlap
            // window and ends inside the next chunk appears as a full span
            // from chunk N and a head-truncated span from chunk N+1 — both
            // have different start_offsets, so start-only keys kept both.
            // Skip a new sentence whose range is contained in an existing one;
            // upgrade an existing head-truncated sentence when the new span is
            // longer (same start, larger end).
            let contained = all
                .iter()
                .any(|s| s.start_offset <= sent.start_offset && s.end_offset >= sent.end_offset);
            if contained {
                continue;
            }
            // Drop any existing sentence fully contained in the new one.
            all.retain(|s| {
                !(sent.start_offset <= s.start_offset && sent.end_offset >= s.end_offset)
            });
            sent.index = all.len();
            all.push(sent);
        }
    }
    // Re-index for a stable sequential id.
    for (i, s) in all.iter_mut().enumerate() {
        s.index = i;
    }
    all
}

/// Convenience: build a mapping from `(chunk_index, local_index)` → [`SentenceId`].
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
        assert!(sents[0].text.contains("关羽"), "first mentions 关羽");
        assert!(sents[1].text.contains("张飞"), "second mentions 张飞");
        assert!(sents[0].end_offset <= sents[1].start_offset);
    }

    /// Objective: Verify that mixed Chinese/English punctuation works.
    /// Invariants: Both 。 and ! are recognized as separators.
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

    /// Objective: Verify that split_all aggregates multiple chunks.
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
        assert_eq!(idx.len(), 3, "three sentences → three entries");
        for (sid, s) in sents.iter().enumerate() {
            let key = (s.chunk_index, s.index);
            assert_eq!(idx[&key], sid, "index maps to position");
        }
    }

    /// Objective: Verify overlapping chunks do not duplicate sentences
    /// (NEW-C21 regression lock).
    /// Invariants: Two chunks sharing a sentence in their overlap region
    /// yield ONE sentence in split_all — never two with the same byte range.
    #[test]
    fn split_all_dedups_overlapping_chunks() {
        // Chunk 0 covers [0..10), chunk 1 covers [8..20): the sentence at
        // [8..10) appears in both.
        let chunks = vec![
            make_chunk(0, "关羽斩华雄。", 0),
            make_chunk(1, "华雄。张飞喝断桥。", 8),
        ];
        let sents = split_all(&chunks);
        let ranges: Vec<(usize, usize)> = sents
            .iter()
            .map(|s| (s.start_offset, s.end_offset))
            .collect();
        let mut unique = ranges.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            ranges.len(),
            unique.len(),
            "no duplicate (start_offset, end_offset) may exist, got {ranges:?}"
        );
        assert!(
            sents.iter().any(|s| s.text.contains("张飞")),
            "non-overlap sentence must still be present"
        );
    }

    /// Objective: Verify sentence offsets point at the TRIMMED text boundaries
    /// (NEW-H26 regression lock) — the document slice must equal the text.
    /// Invariants: For each sentence, a hypothetical
    /// `document[start_offset..end_offset]` slice equals `sentence.text`.
    #[test]
    fn sentence_offsets_match_trimmed_text() {
        // Leading space before the first sentence and trailing whitespace
        // after the separator would break the slice equivalence if offsets
        // pointed at the untrimmed boundaries.
        let chunk = make_chunk(0, " 关羽斩华雄。  张飞喝断桥。", 100);
        let sents = split_chunk(&chunk);
        assert!(!sents.is_empty(), "two sentences expected");
        for s in &sents {
            let document = &chunk.text;
            let local_start = s.start_offset - chunk.start_offset;
            let local_end = s.end_offset - chunk.start_offset;
            let slice = &document[local_start..local_end];
            assert_eq!(
                slice, s.text,
                "document slice must equal sentence text (trimmed offsets), got {slice:?} vs {:?}",
                s.text
            );
        }
    }
}
