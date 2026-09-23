//! Text helpers for the distillation pipeline.

/// Compress a problem-solution pair into a git-commit-style summary.
///
/// Format: a short subject line (the problem core) followed by a body that
/// keeps the causal chain — `Problem: …` then `→ Solution: …` — instead of
/// discarding it. This mirrors how a commit message pairs a one-line subject
/// with a body that records why/how, so a compressed memory still shows what
/// was asked, what was done, and the outcome.
pub fn compress_pair(problem: &str, solution: &str) -> String {
    const MAX_PROBLEM: usize = 60;
    const MAX_BODY_PROBLEM: usize = 160;
    const MAX_SOLUTION: usize = 200;

    let problem = problem.trim();
    let solution = solution.trim();

    if problem.is_empty() && solution.is_empty() {
        return String::new();
    }
    if problem.is_empty() {
        return truncate(solution, MAX_SOLUTION);
    }
    if solution.is_empty() {
        return truncate(problem, MAX_PROBLEM);
    }

    let stripped = problem
        .strip_suffix('?')
        .or_else(|| problem.strip_suffix('？'))
        .map(str::trim)
        .unwrap_or(problem);

    // Subject: the problem core, truncated — one scannable line.
    let subject = truncate(stripped, MAX_PROBLEM);
    // Body: keep the causal chain (what was asked → what was done/result).
    let body_problem = truncate(stripped, MAX_BODY_PROBLEM);
    let action = truncate(solution, MAX_SOLUTION);

    format!("{subject}\nProblem: {body_problem}\n→ Solution: {action}")
}

/// Compute a stable content hash for deduplication.
///
/// Uses FNV-1a because it is dependency-free and fast on short strings.
/// Trims surrounding whitespace so that cosmetic re-formatting does not
/// defeat the dedup check.
pub(super) fn content_hash(s: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut h = FNV_OFFSET;
    for b in s.trim().as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = String::with_capacity(max + 3);
        for c in s.chars() {
            let next_len = out.len() + c.len_utf8();
            if next_len > max {
                out.push('…');
                break;
            }
            out.push(c);
        }
        out
    }
}
