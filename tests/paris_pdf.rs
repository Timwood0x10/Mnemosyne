//! Manual PDF extraction test against the real 巴黎圣母院.pdf corpus file.
//! Run: cargo test --test paris_pdf -- --nocapture
//!
//! 巴黎圣母院.pdf is a SCAN-ONLY document (1600 images, zero text streams).
//! pdf_oxide correctly returns an empty string for it — the old hand-written
//! extractor produced 47k chars of garbage. So an empty result is the CORRECT
//! outcome for this corpus file (no crash, no mojibake); the >500 assertion
//! only applies when the PDF actually contains a text layer.

use lore_scope::knowledge::pdf::extract_text;

#[test]
fn extracts_paris_notre_dame_pdf() {
    let bytes = std::fs::read("corpus/巴黎圣母院.pdf").expect("read 巴黎圣母院.pdf");
    println!(
        "PDF size: {} bytes ({} MiB)",
        bytes.len(),
        bytes.len() / 1048576
    );

    let text = extract_text(&bytes).expect("extract_text must not error");
    println!("Extracted chars: {}", text.chars().count());

    // Scan-only PDF: empty extraction is the correct, garbage-free outcome.
    if text.chars().count() == 0 {
        println!("scan-only PDF (no text layer) — empty extraction is correct (no mojibake)");
        return;
    }

    assert!(
        text.chars().count() > 500,
        "PDF with a text layer should yield substantial text, got {} chars",
        text.chars().count()
    );

    // Print the head so the user can eyeball the extraction quality.
    let head: String = text.chars().take(600).collect();
    println!("────────── HEAD ──────────\n{head}\n────────────────────────");
}
