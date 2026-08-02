//! PDF text extraction, backed by `pdf_oxide`.
//!
//! Delegates to the production-grade `pdf_oxide` crate (MIT/Apache-2.0) which
//! handles CMap/ToUnicode decoding, CJK, reading order, and encrypted/edge-case
//! documents — replacing the earlier hand-rolled `stream … endstream` scanner.
//! The public interface stays a byte-slice in, plain text out, so callers
//! (e.g. `knowledge::format::PdfLoader`) are unaffected.

use crate::error::{Error, Result};

/// Extract concatenated plain text from a PDF byte slice.
///
/// Returns an empty string for a valid PDF with no extractable text. Returns
/// [`Error::InvalidInput`] for non-PDF input, encrypted PDFs, or parse
/// failures.
///
/// # Errors
///
/// - [`Error::InvalidInput`] when the `%PDF` header is missing, the document is
///   encrypted, or pdf_oxide cannot parse the document.
pub fn extract_text(input: &[u8]) -> Result<String> {
    if !input.starts_with(b"%PDF") {
        return Err(Error::InvalidInput(
            "not a PDF file (missing %PDF header)".into(),
        ));
    }
    let mut doc = pdf_oxide::PdfDocument::from_bytes(input.to_vec())
        .map_err(|e| Error::InvalidInput(format!("pdf_oxide parse failed: {e}")))?;
    let page_count = doc
        .page_count()
        .map_err(|e| Error::InvalidInput(format!("pdf_oxide page count failed: {e}")))?;

    let mut out = String::new();
    // Track whether EVERY page failed to extract: a document whose pages all
    // reject extraction (e.g. an encrypted PDF needing a password) must
    // surface as an error, not as a "success" carrying `[page N extraction
    // error]` markers — matching the documented InvalidInput contract.
    let mut all_pages_failed = page_count > 0;
    for page in 0..page_count {
        match doc.extract_text(page) {
            Ok(text) => {
                all_pages_failed = false;
                out.push_str(&text);
                out.push('\n');
            }
            Err(e) => {
                // A page that fails to extract is not fatal when OTHER pages
                // succeed: keep the text we already have and record the page
                // boundary. But if no page yields text, this is a real
                // failure (encrypted / unreadable document).
                out.push_str(&format!("\n[page {page} extraction error: {e}]\n"));
            }
        }
    }
    if all_pages_failed {
        return Err(Error::InvalidInput(
            "pdf_oxide could not extract text from any page (encrypted or unreadable document)"
                .into(),
        ));
    }
    Ok(out.trim().to_string())
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

    /// Build a minimal single-page PDF with a valid cross-reference table,
    /// whose content stream is `content` (compressed with FlateDecode).
    ///
    /// pdf_oxide is a production-grade parser and requires a well-formed xref
    /// table AND a font resource to map glyphs; the earlier hand-rolled
    /// scanner tolerated xref-less/font-less files, so the fixture now emits a
    /// proper 5-object PDF (Catalog/Pages/Page/Contents/Font) with an xref.
    fn build_pdf(content: &str) -> Vec<u8> {
        let compressed = zlib_compress(content.as_bytes());

        // Emit each object into its own buffer so we know its byte offset.
        let obj1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec();
        let obj2 = b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec();
        let obj3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>\nendobj\n".to_vec();
        let mut obj4 = format!(
            "4 0 obj\n<< /Length {} /Filter /FlateDecode >>\nstream\n",
            compressed.len()
        )
        .into_bytes();
        obj4.extend_from_slice(&compressed);
        obj4.extend_from_slice(b"\nendstream\nendobj\n");
        let obj5 =
            b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_vec();

        let header = b"%PDF-1.4\n".to_vec();
        let obj1_start = header.len();
        let obj2_start = obj1_start + obj1.len();
        let obj3_start = obj2_start + obj2.len();

        let mut pdf = header;
        pdf.extend_from_slice(&obj1);
        pdf.extend_from_slice(&obj2);
        pdf.extend_from_slice(&obj3);
        let obj4_start = pdf.len();
        pdf.extend_from_slice(&obj4);
        let obj5_start = pdf.len();
        pdf.extend_from_slice(&obj5);
        let xref_offset = pdf.len();

        // xref table: entry 0 is the free head; entries 1-5 are object
        // offsets (byte positions in `pdf`). All six rows must be present.
        let mut xref = String::new();
        xref.push_str("xref\n0 6\n");
        xref.push_str("0000000000 65535 f \n");
        for off in [obj1_start, obj2_start, obj3_start, obj4_start, obj5_start] {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        xref.push_str(&format!(
            "trailer << /Size 5 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF"
        ));
        pdf.extend_from_slice(xref.as_bytes());
        pdf
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

    /// Objective: Verify damaged/truncated PDF bytes are rejected, not
    /// silently mis-parsed into garbage.
    /// Invariants: A header that claims PDF but has a broken xref → Err.
    #[test]
    fn rejects_damaged_pdf() {
        // Valid %PDF header, but the xref/trailer is truncated away.
        let damaged = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n";
        let err = extract_text(damaged).unwrap_err();
        assert!(
            err.to_string().contains("pdf_oxide"),
            "damaged PDF must surface a parse error, got: {err}"
        );
    }

    /// Objective: Verify a valid PDF with no extractable text yields empty
    /// text rather than an error.
    /// Invariants: A well-formed single-page PDF whose content stream is
    /// empty → Ok(empty string).
    #[test]
    fn empty_pdf_yields_empty_text() {
        let pdf = build_pdf(""); // valid xref + font, but no text operators
        let text = extract_text(&pdf).expect("extract");
        assert!(text.is_empty(), "PDF with no text yields empty string");
    }
}
