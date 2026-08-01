//! Manual PDF extraction test against the real 巴黎圣母院.pdf corpus file.
//! Run: cargo test --test paris_pdf -- --nocapture

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

    assert!(
        text.chars().count() > 500,
        "real 22MiB PDF should yield substantial text, got {} chars",
        text.chars().count()
    );

    // Print the head so the user can eyeball the extraction quality.
    let head: String = text.chars().take(600).collect();
    println!("────────── HEAD ──────────\n{head}\n────────────────────────");
}
