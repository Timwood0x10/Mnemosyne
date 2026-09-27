//! The unified Aho-Corasick lexicon matcher.

use super::*;

/// Per-pattern metadata for the matcher.
struct PatternMeta {
    id: String,
    language: String,
    semantic_class: String,
    /// Require word boundaries around the match (English).
    word_boundary: bool,
}

/// Single-pass matcher over every lexeme form, built once from a registry.
///
/// The Aho-Corasick automaton is constructed once and reused for the lifetime
/// of the matcher — never rebuilt inside compilation loops (P3 requirement).
pub struct LexiconMatcher {
    ac: aho_corasick::AhoCorasick,
    meta: Vec<PatternMeta>,
}

/// Total number of `LexiconMatcher` constructions this process has performed.
///
/// Diagnostic for the P3 lifecycle invariant: the matcher must be built once
/// per process/compile lifecycle, never per sentence or per message. A
/// monotonic jump between identical compiles indicates a rebuild bug.
static MATCHER_BUILDS: AtomicU64 = AtomicU64::new(0);

/// Return how many matchers have been constructed in this process.
pub fn matcher_build_count() -> u64 {
    MATCHER_BUILDS.load(Ordering::Relaxed)
}

impl LexiconMatcher {
    /// Build a matcher from lexemes (all forms included).
    pub fn from_lexemes(lexemes: &[Lexeme]) -> Self {
        MATCHER_BUILDS.fetch_add(1, Ordering::Relaxed);
        let mut patterns: Vec<String> = Vec::new();
        let mut meta: Vec<PatternMeta> = Vec::new();
        for lex in lexemes {
            let all_forms = std::iter::once(&lex.lemma).chain(lex.forms.iter());
            for form in all_forms {
                if form.is_empty() {
                    continue;
                }
                patterns.push(form.clone());
                meta.push(PatternMeta {
                    id: lex.id.clone(),
                    language: lex.language.clone(),
                    semantic_class: lex.semantic_class.clone(),
                    word_boundary: lex.constraints.word_boundary,
                });
            }
        }
        // `MatchKind::LeftmostLongest`, NOT the default `Standard`.
        //
        // Standard reports the match that ENDS earliest, and `find_iter` is
        // non-overlapping: with `no` and `not` sharing a start it reports `no`,
        // the cursor advances past that span, and the word-boundary filter then
        // drops `no` — so `not` is never tried. The shipped dictionary contains
        // 131 such prefix pairs: `not` (swallowed by `no`), all of
        // `began/begin/begins/beginning/begun` (by `be`) and `Mrs`/`Mrs.` (by
        // `Mr`) were unreachable, which silently disabled English negation
        // detection in the matcher the conversation compiler and the title/entity
        // scanner rely on (audit H5). LeftmostLongest resolves the longest form
        // at a position, so a superstring wins over its prefix.
        let ac = aho_corasick::AhoCorasickBuilder::new()
            .match_kind(aho_corasick::MatchKind::LeftmostLongest)
            .build(&patterns)
            .expect("lexeme forms are valid non-empty patterns");
        LexiconMatcher { ac, meta }
    }

    /// Build a matcher from the global registry's merged lexemes.
    pub fn from_global_registry() -> Self {
        let guard = global();
        Self::from_lexemes(guard.lexemes())
    }

    /// Build a matcher restricted to one semantic class (P3: title/entity-kind).
    ///
    /// Used by profile/entity discovery to scan for title markers
    /// (Mr., Prince, 将军…) without matching every verb in the lexicon.
    pub fn from_global_registry_class(class: &str) -> Self {
        let guard = global();
        let lexemes: Vec<Lexeme> = guard.by_class(class).into_iter().cloned().collect();
        Self::from_lexemes(&lexemes)
    }

    /// Iterate matches in `text`, applying word-boundary constraints.
    pub fn find_iter<'a>(&'a self, text: &'a str) -> impl Iterator<Item = LexiconMatch> + 'a {
        self.ac.find_iter(text).filter_map(move |m| {
            let meta = &self.meta[m.pattern()];
            if meta.word_boundary && !is_word_boundary(text, m.start(), m.end()) {
                return None;
            }
            Some(LexiconMatch {
                id: meta.id.clone(),
                matched: text[m.start()..m.end()].to_string(),
                start: m.start(),
                end: m.end(),
                semantic_class: meta.semantic_class.clone(),
                language: meta.language.clone(),
            })
        })
    }

    /// Number of patterns in the automaton (for diagnostics).
    pub fn pattern_count(&self) -> usize {
        self.meta.len()
    }
}

/// Check that both sides of `[start, end)` are non-alphanumeric (word boundary).
fn is_word_boundary(text: &str, start: usize, end: usize) -> bool {
    let before_ok = text[..start]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_alphanumeric());
    let after_ok = text[end..]
        .chars()
        .next()
        .is_none_or(|c| !c.is_alphanumeric());
    before_ok && after_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify a form that is a SUPERSTRING of another form is still
    /// reachable. The automaton used `MatchKind::Standard`, which reports the
    /// match ending earliest: at a shared start (`no` / `not`) it reported `no`,
    /// the non-overlapping iterator advanced past that span, and the
    /// word-boundary filter then discarded `no` — leaving `not` unreachable. The
    /// matcher backs negation detection in the conversation compiler (audit H5).
    /// Invariants: the longer form is matched, and the reported span covers it.
    #[test]
    fn a_longer_form_is_not_shadowed_by_its_prefix() {
        let matcher = LexiconMatcher::from_global_registry();
        let matched: Vec<String> = matcher
            .find_iter("I do not care")
            .map(|m| m.matched)
            .collect();
        assert!(
            matched.iter().any(|m| m == "not"),
            "the `not` form must be reachable, got {matched:?}"
        );

        // `no` must still match on its own — the fix must not trade one form for
        // the other.
        let bare: Vec<String> = matcher.find_iter("no idea").map(|m| m.matched).collect();
        assert!(
            bare.iter().any(|m| m == "no"),
            "`no` must keep matching when `not` is absent, got {bare:?}"
        );
    }

    /// Objective: Verify the word-boundary filter still rejects a substring whose
    /// edges fall inside a longer word, so the longer-match preference did not
    /// loosen that contract.
    /// Invariants: a boundary-constrained form inside a word is dropped.
    #[test]
    fn boundary_constrained_forms_do_not_match_inside_words() {
        let matcher = LexiconMatcher::from_global_registry();
        let matched: Vec<String> = matcher
            .find_iter("the notebook is here")
            .map(|m| m.matched)
            .collect();
        assert!(
            !matched.iter().any(|m| m == "not"),
            "`not` inside `notebook` must stay rejected, got {matched:?}"
        );
    }
}
