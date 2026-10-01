// Diagnostic: count chapters produced by split_into_chapters per novel,
// plus a sample of chapter numbers, to verify splitting is correct.
//
// Run: cargo test --test chap_diag -- --ignored --nocapture
use mnemosyne::ingest::corpus;

/// Objective: Report per-novel chapter counts and flag gaps or duplicates left
/// by `split_into_chapters`.
/// Invariants: every listed novel loads from `corpus/` (a failure is a panic,
/// not a silent skip) and the printed chapter range is contiguous unless a gap
/// is reported.
#[test]
#[ignore]
fn diag_chapter_counts() {
    // Diagnostics go through `tracing` (the crate's tests must not print);
    // run with `--nocapture` to see them.
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();
    for novel in &["水浒传", "三国演义", "红楼梦", "西游记"] {
        let chapters = corpus::load_novel(novel, std::path::Path::new("corpus"))
            .unwrap_or_else(|e| panic!("load {novel}: {e}"));
        let nums: Vec<i32> = chapters.iter().map(|c| c.num).collect();
        let first = nums.first().copied();
        let last = nums.last().copied();
        // Detect gaps or duplicates in the chapter numbering.
        let mut sorted = nums.clone();
        sorted.sort_unstable();
        let mut gaps = Vec::new();
        for w in sorted.windows(2) {
            if w[1] != w[0] + 1 {
                gaps.push((w[0], w[1]));
            }
        }
        tracing::info!(
            "{novel}: {} chapters, first={first:?} last={last:?} gaps={:?}",
            chapters.len(),
            gaps
        );
        // Sample a few chapter text lengths to spot 1-mega-chapter collapse.
        let mut lens: Vec<usize> = chapters.iter().map(|c| c.text.len()).collect();
        lens.sort_unstable();
        let median = lens.get(lens.len() / 2).copied().unwrap_or(0);
        let max = lens.last().copied().unwrap_or(0);
        tracing::info!("  text len bytes: median={median}, max={max}");
    }
}
