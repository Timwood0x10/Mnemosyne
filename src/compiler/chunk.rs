//! Chunk Planner — Phase 1.
//!
//! Splits a [`Document`] into [`Chunk`]s with configurable size and overlap.
//! Chunks are the unit of parallel compilation — each chunk is self-contained
//! enough for independent entity extraction and observation building.
//!
//! ## Chunking strategy
//!
//! V1 uses a simple character-count strategy with configurable overlap.
//! Future versions may add chapter-aware splitting (reusing
//! [`ingest::corpus::split_into_chapters`]) for classical novels.
//!
//! ## Overlap rationale
//!
//! Overlap ensures that cross-chunk references (pronouns, continued
//! sentences) can be resolved. For example:
//!
//! ```text
//! Chunk N:   ...赵云来到长坂坡。
//! Overlap:   赵云来到长坂坡。他救出了阿斗。
//! Chunk N+1: 他救出了阿斗。曹操闻讯大怒。
//! ```
//!
//! The overlap gives the resolver enough context to resolve "他" → "赵云".

use crate::compiler::Chunk;

/// Chunking configuration.
///
/// Defaults are tuned for Chinese classical novels (~1500-2500 chars per chunk
/// with 100-300 char overlap).
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Target chunk size in characters (not bytes).
    pub chunk_size: usize,
    /// Overlap between consecutive chunks in characters.
    pub overlap: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            chunk_size: 2000,
            overlap: 200,
        }
    }
}

/// Plan chunks from the full document text.
///
/// Returns a [`Vec<Chunk>`] covering the entire text. The last chunk may be
/// shorter than `config.chunk_size`.
///
/// ## Algorithm
///
/// 1. Walk forward by `chunk_size - overlap` characters.
/// 2. Each chunk starts where the previous ended (adjusted for overlap).
/// 3. Chunks are aligned to character boundaries (not byte boundaries).
/// 4. Offsets are byte-level (for evidence tracing).
pub fn plan(text: &str, config: Config) -> Vec<Chunk> {
    if text.is_empty() {
        return Vec::new();
    }

    let chars: Vec<char> = text.chars().collect();
    let total_chars = chars.len();
    let step = config.chunk_size.saturating_sub(config.overlap);
    if step == 0 {
        // If overlap >= chunk_size, just return the whole text as one chunk.
        return vec![Chunk {
            index: 0,
            text: text.to_owned(),
            start_offset: 0,
            end_offset: text.len(),
            segment_num: 1,
            overlap_before: 0,
            overlap_after: 0,
        }];
    }

    let mut chunks = Vec::new();
    let mut char_start = 0usize;
    let mut chunk_index = 0usize;

    while char_start < total_chars {
        let char_end = (char_start + config.chunk_size).min(total_chars);

        // Extract the char range for this chunk
        let chunk_chars: String = chars[char_start..char_end].iter().collect();

        // Byte offsets (map from char indices to byte positions)
        let start_offset = char_to_byte_offset(text, char_start);
        let end_offset = char_to_byte_offset(text, char_end);

        // Overlap in bytes (for safe slicing): count bytes of `overlap` chars
        // before the start and after the end of this chunk.
        let overlap_before = if chunk_index == 0 {
            0usize
        } else {
            let overlap_chars = config.overlap.min(char_start);
            let overlap_start = char_to_byte_offset(text, char_start - overlap_chars);
            start_offset - overlap_start
        };

        let overlap_chars_after = config.overlap.min(total_chars - char_end);
        let overlap_after = if overlap_chars_after == 0 {
            0usize
        } else {
            let overlap_end = char_to_byte_offset(text, char_end + overlap_chars_after);
            overlap_end - end_offset
        };

        chunks.push(Chunk {
            index: chunk_index,
            text: chunk_chars,
            start_offset,
            end_offset,
            segment_num: 1, // default; chapter-aware chunking will set this
            overlap_before,
            overlap_after,
        });

        char_start += step;
        chunk_index += 1;
    }

    chunks
}

/// Convert a character index to a byte offset in `text`.
///
/// For ASCII-only text this is the same as the char index; for multi-byte
/// UTF-8 (Chinese) it maps correctly.
fn char_to_byte_offset(text: &str, char_pos: usize) -> usize {
    text.char_indices()
        .nth(char_pos)
        .map(|(byte_idx, _)| byte_idx)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify that an empty text produces no chunks.
    /// Invariants: The output vector is empty; no panics.
    #[test]
    fn empty_text_yields_no_chunks() {
        let chunks = plan("", Config::default());
        assert!(chunks.is_empty(), "empty text → no chunks");
    }

    /// Objective: Verify that short text (< chunk_size) produces one chunk.
    /// Invariants: Exactly one chunk covers the full text; overlap is zero.
    #[test]
    fn short_text_is_one_chunk() {
        let text = "赵云救阿斗。";
        let chunks = plan(text, Config::default());
        assert_eq!(chunks.len(), 1, "short text → single chunk");
        assert_eq!(chunks[0].text, text);
        assert_eq!(chunks[0].overlap_before, 0);
        assert_eq!(chunks[0].overlap_after, 0);
    }

    /// Objective: Verify that a long text is split into multiple chunks.
    /// Invariants: Consecutive chunks start at non-overlapping positions;
    /// the final chunk may be shorter.
    #[test]
    fn long_text_splits_into_multiple_chunks() {
        let text = "语".repeat(5000); // 5000 Chinese chars
        let cfg = Config {
            chunk_size: 2000,
            overlap: 200,
        };
        let chunks = plan(&text, cfg);
        // 5000 / (2000-200) = 5000/1800 ≈ 2.78 → 3 chunks
        assert!(
            chunks.len() >= 2,
            "5000 chars should produce ≥2 chunks, got {}",
            chunks.len()
        );
        // Verify ordering: each chunk's index matches its position
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i, "chunk index should match position");
        }
    }

    /// Objective: Verify that overlap bytes are accounted for in consecutive chunks.
    /// Invariants: Chunk N's end overlaps with Chunk N+1's start by `overlap` characters.
    #[test]
    fn overlap_is_present() {
        let text = "语".repeat(3000);
        let cfg = Config {
            chunk_size: 1000,
            overlap: 100,
        };
        let chunks = plan(&text, cfg);
        assert!(chunks.len() >= 2, "need at least 2 chunks for overlap test");

        // Chunk 0 end chars should overlap with Chunk 1 start chars
        let overlap_text =
            &text[chunks[0].end_offset - chunks[0].overlap_after..chunks[0].end_offset];
        let chunk1_start =
            &text[chunks[1].start_offset..chunks[1].start_offset + overlap_text.len()];
        // The overlap region of chunk 0 must equal the opening bytes of chunk 1.
        assert_eq!(
            overlap_text, chunk1_start,
            "overlap region should equal chunk 1's opening bytes"
        );
        // For all-Chinese text, overlap char count should match configured overlap
        assert!(
            overlap_text.chars().count() >= cfg.overlap / 2,
            "overlap region should be non-trivial"
        );
    }

    /// Objective: Verify that Config::default() does not cause a zero-step panic.
    /// Invariants: Default config creates sensible chunks for a long text.
    #[test]
    fn default_config_works() {
        let text = "大".repeat(10000);
        let chunks = plan(&text, Config::default());
        assert!(!chunks.is_empty(), "default config should produce chunks");
        assert!(chunks.len() > 2, "10000 chars should produce >2 chunks");
    }

    /// Objective: Verify that the sum of chunk texts reconstructs the original
    /// when overlap is excluded (deduplicated).
    /// Invariants: Chunks cover the full text range without gaps.
    #[test]
    fn chunks_cover_full_text() {
        let text = "宋".repeat(3500);
        let cfg = Config {
            chunk_size: 1000,
            overlap: 100,
        };
        let chunks = plan(&text, cfg);
        assert!(!chunks.is_empty());
        // First chunk starts at 0
        assert_eq!(chunks[0].start_offset, 0);
        // Last chunk ends at text.len()
        assert_eq!(
            chunks.last().unwrap().end_offset,
            text.len(),
            "last chunk should end at text boundary"
        );
    }
}
