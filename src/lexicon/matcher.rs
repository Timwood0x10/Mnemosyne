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
        let ac = aho_corasick::AhoCorasick::new(&patterns)
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
