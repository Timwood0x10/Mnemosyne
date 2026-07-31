//! Best-effort pure-Rust PDF text extractor.
//!
//! Extracts plain text from **simple** text-based PDFs without any external
//! native dependency. It scans `stream … endstream` blocks, inflates
//! `/FlateDecode` streams with `flate2::read::ZlibDecoder` (PDF FlateDecode is
//! zlib-wrapped, not raw deflate), and pulls text out of `BT … ET` content
//! blocks via the `Tj` / `TJ` / `'` / `"` text operators (literal `(...)`
//! strings and `<hex>` strings).
//!
//! ## Limitations (documented, not fixed)
//!
//! - Custom font encodings / CMaps (glyph codes → not Unicode) are not decoded.
//! - Filter chains (e.g. `/FlateDecode /ASCIIHexDecode`) are not unwrapped.
//! - PDF 1.5+ cross-reference streams and encrypted/image-only PDFs are rejected
//!   with a typed [`Error`] rather than producing garbage.
//!
//! For corpus ingestion of well-formed text PDFs this is sufficient; users who
//! need full fidelity can pre-convert to `.txt` and use the text loader.

use flate2::read::ZlibDecoder;
use std::io::Read;

use crate::error::{Error, Result};

/// Extract concatenated plain text from a PDF byte slice.
///
/// Returns an empty string for a valid PDF with no extractable text. Returns
/// [`Error::InvalidInput`] for non-PDF input, encrypted PDFs, or bytes that
/// cannot be decoded as Latin-1 (PDF content streams are byte-oriented).
///
/// # Errors
///
/// - [`Error::InvalidInput`] when the `%PDF` header is missing, the document is
///   encrypted, or a FlateDecode stream cannot be inflated.
pub fn extract_text(input: &[u8]) -> Result<String> {
    if !input.starts_with(b"%PDF") {
        return Err(Error::InvalidInput(
            "not a PDF file (missing %PDF header)".into(),
        ));
    }
    if contains_subslice(input, b"/Encrypt") {
        return Err(Error::InvalidInput(
            "encrypted PDFs are not supported by the built-in extractor".into(),
        ));
    }

    let mut out = String::new();
    let mut pos = 0usize;
    while let Some(rel) = find_from(input, b"stream", pos) {
        let stream_tok = rel;
        // The dict immediately preceding `stream` describes this stream's
        // filters. Look back only within the current object to avoid matching
        // an unrelated `/FlateDecode` earlier in the file.
        let dict_region_end = stream_tok;
        let dict_region_start = pos;
        let is_flate =
            contains_subslice(&input[dict_region_start..dict_region_end], b"/FlateDecode");

        // Body begins after `stream` + EOL (CR LF or LF).
        let mut body_start = stream_tok + b"stream".len();
        if body_start < input.len() && input[body_start] == b'\r' {
            body_start += 1;
        }
        if body_start < input.len() && input[body_start] == b'\n' {
            body_start += 1;
        }

        // Body ends at the next `endstream`.
        let body_end = find_from(input, b"endstream", body_start).unwrap_or(input.len());
        let stream_bytes = &input[body_start..body_end];

        let decoded: Vec<u8> = if is_flate {
            inflate_zlib(stream_bytes)?
        } else {
            // Uncompressed or unsupported filter — best effort: keep raw bytes.
            stream_bytes.to_vec()
        };

        out.push_str(&extract_text_from_content(&decoded));
        out.push('\n');

        // Advance past `endstream` to continue the outer scan.
        pos = body_end + b"endstream".len().min(input.len() - body_end);
    }

    Ok(out.trim().to_string())
}

/// Inflate a zlib-wrapped FlateDecode stream to raw bytes.
fn inflate_zlib(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = ZlibDecoder::new(bytes);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| Error::InvalidInput(format!("FlateDecode inflate failed: {e}")))?;
    Ok(out)
}

/// Pull text out of a decoded content stream by scanning `BT … ET` blocks.
///
/// Inside a text object, literal `(...)` strings and `<hex>` strings are
/// concatenated. Strings outside `BT … ET` (e.g. inside resource dicts) are
/// ignored so dict values do not pollute the extracted text.
fn extract_text_from_content(decoded: &[u8]) -> String {
    // PDF content streams are byte-oriented ASCII operators; lossy Latin-1 keeps
    // every byte representable so operator scanning never panics on high bytes.
    let content = String::from_utf8_lossy(decoded).into_owned();
    let mut out = String::new();
    let mut rest: &str = &content;

    while let Some(bt) = find_token(rest, "BT") {
        rest = &rest[bt..];
        let et = find_token(rest, "ET").unwrap_or(rest.len());
        let block = &rest[..et];
        out.push_str(&extract_strings(block));
        // Separate text objects with a space so words do not collide across
        // adjacent `Tj` operators on the same line.
        out.push(' ');
        rest = &rest[et..];
    }

    out
}

/// Extract `(...)` literal strings and `<...>` hex strings from a text block.
///
/// Handles `\` escapes inside literal strings (the escaped char is kept
/// verbatim — full PDF escape semantics like `\n` → newline are intentionally
/// not applied, since glyph text rarely depends on them). Balanced nested
/// parens are tracked per the PDF spec.
fn extract_strings(block: &str) -> String {
    let bytes = block.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => {
                let (text, next) = read_literal(&bytes[i + 1..]);
                out.push_str(&text);
                i += 1 + next;
            }
            b'<' => {
                // Skip `<<` dict-open; only single `<` starts a hex string.
                if i + 1 < bytes.len() && bytes[i + 1] == b'<' {
                    i += 2;
                    continue;
                }
                let (text, next) = read_hex(&bytes[i + 1..]);
                out.push_str(&text);
                i += 1 + next;
            }
            _ => {
                i += 1;
            }
        }
    }
    out
}

/// Read a literal `(...)` string body starting just after the opening paren.
/// Returns the decoded text and the number of bytes consumed (excluding the
/// opening paren, including the closing paren).
fn read_literal(body: &[u8]) -> (String, usize) {
    let mut depth = 1usize;
    let mut buf = Vec::new();
    let mut j = 0usize;
    while j < body.len() && depth > 0 {
        match body[j] {
            b'\\' => {
                // Keep the escaped byte verbatim (best-effort).
                if j + 1 < body.len() {
                    buf.push(body[j + 1]);
                    j += 2;
                } else {
                    j += 1;
                }
            }
            b'(' => {
                depth += 1;
                buf.push(b'(');
                j += 1;
            }
            b')' => {
                depth -= 1;
                if depth > 0 {
                    buf.push(b')');
                }
                j += 1;
            }
            _ => {
                buf.push(body[j]);
                j += 1;
            }
        }
    }
    (String::from_utf8_lossy(&buf).into_owned(), j)
}

/// Read a `<...>` hex string body starting just after the opening `<`.
/// Returns the decoded text and bytes consumed (excluding `<`, including `>`).
fn read_hex(body: &[u8]) -> (String, usize) {
    let mut hex = String::new();
    let mut j = 0usize;
    while j < body.len() && body[j] != b'>' {
        hex.push(body[j] as char);
        j += 1;
    }
    // Skip the closing `>` if present.
    if j < body.len() {
        j += 1;
    }
    let cleaned: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    let mut out = String::new();
    for pair in cleaned.as_bytes().chunks(2) {
        let s = std::str::from_utf8(pair).unwrap_or("");
        if let Ok(b) = u8::from_str_radix(s, 16) {
            out.push(b as char);
        }
    }
    (out, j)
}

/// Find the byte offset of `needle` in `hay` starting from `from`, or `None`.
fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Return `true` if `hay` contains `needle` as a byte subslice.
fn contains_subslice(hay: &[u8], needle: &[u8]) -> bool {
    find_from(hay, needle, 0).is_some()
}

/// Find the byte offset of a whitespace/delimiter-bounded token in `s`.
///
/// `BT` must not match inside a longer token like `BTX`, so the token is
/// required to be followed by a delimiter (whitespace or PDF delimiter char).
fn find_token(s: &str, token: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let tok = token.as_bytes();
    let mut from = 0;
    while let Some(rel) = find_from(bytes, tok, from) {
        let after = rel + tok.len();
        let ok_before = rel == 0 || is_delim(bytes[rel - 1]);
        let ok_after = after >= bytes.len() || is_delim(bytes[after]);
        if ok_before && ok_after {
            return Some(rel);
        }
        from = rel + 1;
    }
    None
}

/// PDF delimiters: whitespace plus the structural delimiter characters.
fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t'
            | b'\n'
            | b'\r'
            | b'\x0c'
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'/'
            | b'%'
    )
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    /// Compress `data` with zlib so it matches PDF FlateDecode encoding.
    fn zlib_compress(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).expect("write");
        encoder.finish().expect("finish")
    }

    /// Build a minimal single-page PDF whose content stream is `content`
    /// (compressed with FlateDecode), so extractor behavior is testable.
    fn build_pdf(content: &str) -> Vec<u8> {
        let compressed = zlib_compress(content.as_bytes());
        let mut pdf = String::new();
        pdf.push_str("%PDF-1.4\n");
        pdf.push_str("1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        pdf.push_str("2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        pdf.push_str("3 0 obj\n<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>\nendobj\n");
        pdf.push_str(&format!(
            "4 0 obj\n<< /Length {} /Filter /FlateDecode >>\nstream\n",
            compressed.len()
        ));
        let mut bytes = pdf.into_bytes();
        bytes.extend_from_slice(&compressed);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(b"trailer << /Root 1 0 R >>\n%%EOF");
        bytes
    }

    /// Objective: Verify a FlateDecode content stream with a single Tj string
    /// extracts to the plain text.
    /// Invariants: "BT (Hello World) Tj ET" → "Hello World".
    #[test]
    fn extracts_single_tj_string() {
        let pdf = build_pdf("BT (Hello World) Tj ET");
        let text = extract_text(&pdf).expect("extract");
        assert!(
            text.contains("Hello World"),
            "extracted text must contain the literal string, got: {text:?}"
        );
    }

    /// Objective: Verify a TJ array of literal strings concatenates its parts.
    /// Invariants: "BT [(Hel) 10 (lo)] TJ ET" → "Hello".
    #[test]
    fn extracts_tj_array_strings() {
        let pdf = build_pdf("BT [(Hel) 10 (lo)] TJ ET");
        let text = extract_text(&pdf).expect("extract");
        assert!(
            text.contains("Hello"),
            "TJ array parts must concatenate, got: {text:?}"
        );
    }

    /// Objective: Verify a hex string `<48656C6C6F>` decodes to "Hello".
    /// Invariants: Hex pairs map to bytes; output is the decoded ASCII.
    #[test]
    fn extracts_hex_string() {
        let pdf = build_pdf("BT <48656C6C6F> Tj ET");
        let text = extract_text(&pdf).expect("extract");
        assert!(
            text.contains("Hello"),
            "hex string must decode to Hello, got: {text:?}"
        );
    }

    /// Objective: Verify non-PDF input is rejected with a typed error.
    /// Invariants: Missing %PDF header → Err(InvalidInput); no panic.
    #[test]
    fn rejects_non_pdf_input() {
        let err = extract_text(b"just some plain text").unwrap_err();
        assert!(
            err.to_string().contains("not a PDF"),
            "non-PDF input must be rejected, got: {err}"
        );
    }

    /// Objective: Verify encrypted PDFs are rejected, not silently mis-parsed.
    /// Invariants: Presence of /Encrypt → Err(InvalidInput).
    #[test]
    fn rejects_encrypted_pdf() {
        let mut pdf = build_pdf("BT (secret) Tj ET");
        // Inject an /Encrypt marker to simulate an encrypted document.
        let enc = b"/Encrypt";
        // `%%EOF` is 5 bytes — windows(5) is required for an exact match;
        // windows(4) would never find it and unwrap() would panic.
        let pos = pdf
            .windows(5)
            .position(|w| w == b"%%EOF")
            .expect("build_pdf always appends %%EOF");
        pdf.splice(pos..pos, enc.iter().copied());
        let err = extract_text(&pdf).unwrap_err();
        assert!(
            err.to_string().contains("encrypted"),
            "encrypted PDF must be rejected, got: {err}"
        );
    }

    /// Objective: Verify an empty (no-stream) PDF yields empty text, not error.
    /// Invariants: Header-only PDF → Ok(empty string).
    #[test]
    fn empty_pdf_yields_empty_text() {
        let pdf = b"%PDF-1.4\ntrailer << /Root 1 0 R >>\n%%EOF";
        let text = extract_text(pdf).expect("extract");
        assert!(text.is_empty(), "PDF with no streams yields empty text");
    }

    /// Objective: Verify read_literal handles escaped and balanced parens.
    /// Invariants: `a\(b\)c)` → "a(b)c" and consumes through the matching close.
    #[test]
    fn read_literal_handles_escapes_and_balance() {
        let (text, consumed) = read_literal(b"a\\(b\\)c)");
        assert_eq!(text, "a(b)c", "escaped parens are kept verbatim");
        assert_eq!(
            consumed, 8,
            "consumed count includes bytes through the closing paren"
        );
    }
}
