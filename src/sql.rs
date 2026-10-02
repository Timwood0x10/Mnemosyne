//! Shared SQLite text and limit helpers used by every store.
//!
//! Both helpers encode a rule that is easy to get subtly wrong and that was, in
//! fact, wrong in one store while right in the others — the character store
//! escaped `%`/`_` before `\` and silently dropped matches for queries ending in
//! a backslash, and two stores clamped `LIMIT` while three did not. Keeping the
//! rule in one place is what stops that from happening again.

/// Largest row limit a store submits to SQLite in a `LIMIT`/`k` clause.
pub const MAX_SQL_LIMIT: usize = 10_000;

/// Clamp a caller-supplied row limit before the `i64` cast.
///
/// A `usize` above `i64::MAX` wraps to a negative `i64`, and SQLite reads
/// `LIMIT -1` as "no limit" — the oversized request would silently become a
/// whole-table scan. Only values that reach SQL are clamped; in-memory
/// truncation keeps the caller's own limit.
///
/// # Examples
///
/// ```
/// # use mnemosyne::sql::sql_limit;
/// assert_eq!(sql_limit(usize::MAX), 10_000);
/// assert_eq!(sql_limit(250), 250);
/// ```
#[must_use]
pub fn sql_limit(limit: usize) -> i64 {
    limit.min(MAX_SQL_LIMIT) as i64
}

/// Escape `query` so it matches literally inside a `LIKE ... ESCAPE '\'`
/// comparison, without the surrounding wildcards.
///
/// Use this when the SQL supplies the wildcards itself (`LIKE '%' || ?1 || '%'
/// ESCAPE '\'`); use [`like_pattern`] when the pattern is bound whole.
///
/// The backslash MUST be escaped before `%` and `_`: escaping the wildcards
/// first leaves a lone `\` for the next step to double, so a query already
/// ending in `\` would otherwise combine with an appended trailing `%` — the
/// pattern `%foo\%` makes that wildcard a literal percent sign and silently
/// drops the "ends-with" match.
///
/// # Examples
///
/// ```
/// # use mnemosyne::sql::escape_like;
/// assert_eq!(escape_like("50%"), r"50\%");
/// assert_eq!(escape_like("a_b"), r"a\_b");
/// assert_eq!(escape_like("C:\\"), r"C:\\");
/// ```
#[must_use]
pub fn escape_like(query: &str) -> String {
    query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Build a whole `LIKE` pattern (`%query%`) with every metacharacter escaped.
///
/// Equivalent to wrapping [`escape_like`] in `%…%`; see that function for why
/// the escape order matters.
///
/// # Examples
///
/// ```
/// # use mnemosyne::sql::like_pattern;
/// assert_eq!(like_pattern("foo"), "%foo%");
/// assert_eq!(like_pattern("50%"), r"%50\%%");
/// ```
#[must_use]
pub fn like_pattern(query: &str) -> String {
    format!("%{}%", escape_like(query))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify `like_pattern` escapes the backslash BEFORE `%`/`_`,
    /// so a query ending in `\` keeps its trailing wildcard (the old order
    /// produced `%foo\%`, whose trailing wildcard became a literal percent
    /// sign and dropped the match).
    /// Invariants: each metacharacter is backslash-escaped and the surrounding
    /// `%…%` wildcards survive.
    #[test]
    fn like_pattern_escapes_backslash_before_wildcards() {
        assert_eq!(
            like_pattern("foo\\"),
            r"%foo\\%",
            "trailing backslash must be doubled, not eat the trailing wildcard"
        );
        assert_eq!(like_pattern("50%"), r"%50\%%", "percent must be escaped");
        assert_eq!(like_pattern("a_b"), r"%a\_b%", "underscore must be escaped");
        assert_eq!(
            like_pattern(r"a\%_"),
            r"%a\\\%\_%",
            "backslash, percent and underscore must all escape independently"
        );
    }

    /// Objective: Verify `escape_like` returns the escaped body WITHOUT adding
    /// wildcards, for callers whose SQL concatenates them.
    /// Invariants: the output contains no unescaped metacharacter and no
    /// surrounding `%`.
    #[test]
    fn escape_like_omits_the_wrapping_wildcards() {
        assert_eq!(escape_like("plain"), "plain", "nothing to escape");
        assert_eq!(escape_like("50%"), r"50\%", "percent escaped, not wrapped");
        assert_eq!(
            escape_like("C:\\"),
            r"C:\\",
            "a trailing backslash doubles without swallowing a wildcard"
        );
        assert!(
            !escape_like("x").starts_with('%'),
            "the helper must not add a leading wildcard"
        );
    }

    /// Objective: Verify an oversized row limit cannot reach SQLite as a
    /// negative number, where `LIMIT -1` would silently mean "no limit".
    /// Invariants: the ceiling and everything above it clamp to
    /// [`MAX_SQL_LIMIT`]; in-range values pass through; the result is never
    /// negative.
    #[test]
    fn sql_limit_clamps_oversized_requests() {
        assert_eq!(
            sql_limit(usize::MAX),
            MAX_SQL_LIMIT as i64,
            "usize::MAX must clamp to the ceiling instead of wrapping negative"
        );
        assert_eq!(
            sql_limit(MAX_SQL_LIMIT + 1),
            MAX_SQL_LIMIT as i64,
            "one above the ceiling must clamp too"
        );
        assert_eq!(
            sql_limit(MAX_SQL_LIMIT),
            MAX_SQL_LIMIT as i64,
            "the ceiling itself is allowed"
        );
        assert_eq!(
            sql_limit(250),
            250,
            "in-range limits pass through unchanged"
        );
        assert_eq!(sql_limit(0), 0, "zero must stay zero (empty result set)");
    }
}
