//! Language Provider — encapsulates language-specific patterns for the compiler.
//!
//! The compiler pipeline is language-agnostic. All language-specific behaviour
//! (sentence splitting, profile extraction patterns, entity name discovery)
//! is provided through the [`LanguageProvider`] trait. Supporting a new language
//! means implementing this trait — no compiler code changes needed.

/// A set of language-specific patterns used by the compiler pipeline.
///
/// Each method returns the patterns that the compiler should use when
/// processing text in this language. The defaults are empty — implementations
/// override only what they need.
pub trait LanguageProvider: Send + Sync {
    /// Name of this language (e.g. "chinese", "english").
    fn name(&self) -> &str;

    /// Characters that end a sentence for this language.
    fn sentence_separators(&self) -> &[char] {
        &['。', '！', '？', '；', '.', '!', '?', ';', '\n']
    }

    /// Profile extraction patterns: (pattern, key, mode, suffix/fallback).
    /// Mode: "After" / "Before" / "Between" / "Until"
    fn profile_patterns(&self) -> &[ProfilePatternDef] {
        &[]
    }

    /// Substring markers used to discover entity names from text.
    /// If the text contains one of these markers, the text before it is
    /// taken as a candidate entity name.
    fn discovery_markers(&self) -> &[&str] {
        &[]
    }

    /// Characters that mark the boundary of a profile value.
    fn profile_stop_chars(&self) -> &[char] {
        &['，', ',', '。', '；', '、', '\n', '：']
    }

    /// Characters that stop the entity name walking in auto-discovery.
    fn discovery_stop_chars(&self) -> &[char] {
        &['名', '姓']
    }

    /// Default chapter heading pattern regex (for chapter number detection).
    fn chapter_pattern(&self) -> &str {
        "第"
    }

    /// Strong action verbs for event extraction (e.g. 杀, killed, captured).
    fn strong_verbs(&self) -> Vec<String> {
        Vec::new()
    }

    /// Weaker action / motion / emotion verbs (e.g. 大怒, exclaimed, wept).
    fn action_verbs(&self) -> Vec<String> {
        Vec::new()
    }

    /// Verbs that indicate hostile relationship changes (杀, attacked, killed).
    fn hostile_verbs(&self) -> Vec<String> {
        Vec::new()
    }

    /// Verbs that indicate friendly relationship changes (救, saved, blessed).
    fn friendly_verbs(&self) -> Vec<String> {
        Vec::new()
    }
}

/// A single profile extraction pattern definition.
#[derive(Debug, Clone)]
pub struct ProfilePatternDef {
    pub pattern: &'static str,
    pub key: &'static str,
    pub mode: &'static str,
    pub suffix: Option<&'static str>,
}

// ── Chinese Language Provider ───────────────────────────────────────────────

/// Chinese-specific patterns for the compiler.
pub struct ChineseLanguageProvider;

impl ChineseLanguageProvider {
    pub fn new() -> Self {
        ChineseLanguageProvider
    }
}

impl Default for ChineseLanguageProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageProvider for ChineseLanguageProvider {
    fn name(&self) -> &str {
        "chinese"
    }

    fn sentence_separators(&self) -> &[char] {
        &['。', '！', '？', '；', '\n']
    }

    fn profile_patterns(&self) -> &[ProfilePatternDef] {
        &[
            ProfilePatternDef {
                pattern: "字",
                key: "courtesy_name",
                mode: "After",
                suffix: None,
            },
            ProfilePatternDef {
                pattern: "人也",
                key: "birthplace",
                mode: "Before",
                suffix: None,
            },
            ProfilePatternDef {
                pattern: "之后",
                key: "ancestry",
                mode: "Before",
                suffix: None,
            },
            ProfilePatternDef {
                pattern: "身长",
                key: "appearance_height",
                mode: "Between",
                suffix: Some("尺"),
            },
            ProfilePatternDef {
                pattern: "面如",
                key: "appearance_face",
                mode: "After",
                suffix: None,
            },
            ProfilePatternDef {
                pattern: "为业",
                key: "occupation",
                mode: "Before",
                suffix: None,
            },
            ProfilePatternDef {
                pattern: "使",
                key: "weapon",
                mode: "Until",
                suffix: Some("，"),
            },
            ProfilePatternDef {
                pattern: "姓",
                key: "surname",
                mode: "Between",
                suffix: Some("名"),
            },
            ProfilePatternDef {
                pattern: "名",
                key: "given_name",
                mode: "BeforeWithFallback",
                suffix: Some("字"),
            },
            ProfilePatternDef {
                pattern: "号",
                key: "title",
                mode: "After",
                suffix: None,
            },
            ProfilePatternDef {
                pattern: "威风",
                key: "demeanor",
                mode: "After",
                suffix: None,
            },
        ]
    }

    fn discovery_markers(&self) -> &[&str] {
        &["字", "者也", "身长", "面如", "使", "姓", "号", "威风"]
    }

    fn profile_stop_chars(&self) -> &[char] {
        &['，', ',', '。', '；', '、', '\n', '：']
    }

    fn discovery_stop_chars(&self) -> &[char] {
        &['名', '姓']
    }

    fn chapter_pattern(&self) -> &str {
        "第"
    }

    fn strong_verbs(&self) -> Vec<String> {
        crate::dictionary::global()
            .chinese_strong_verbs()
            .iter()
            .cloned()
            .collect()
    }

    fn action_verbs(&self) -> Vec<String> {
        crate::dictionary::global()
            .chinese_action_verbs()
            .iter()
            .cloned()
            .collect()
    }

    fn hostile_verbs(&self) -> Vec<String> {
        crate::dictionary::global()
            .chinese_hostile_verbs()
            .iter()
            .cloned()
            .collect()
    }

    fn friendly_verbs(&self) -> Vec<String> {
        crate::dictionary::global()
            .chinese_friendly_verbs()
            .iter()
            .cloned()
            .collect()
    }
}

// ── English Language Provider ───────────────────────────────────────────────

/// English-specific patterns for the compiler.
pub struct EnglishLanguageProvider;

impl EnglishLanguageProvider {
    pub fn new() -> Self {
        EnglishLanguageProvider
    }
}

impl Default for EnglishLanguageProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageProvider for EnglishLanguageProvider {
    fn name(&self) -> &str {
        "english"
    }

    fn sentence_separators(&self) -> &[char] {
        &['.', '!', '?', ';', '\n']
    }

    fn profile_patterns(&self) -> &[ProfilePatternDef] {
        &[
            ProfilePatternDef {
                pattern: "was the son of",
                key: "parentage",
                mode: "Before",
                suffix: None,
            },
            ProfilePatternDef {
                pattern: "was the daughter of",
                key: "parentage",
                mode: "Before",
                suffix: None,
            },
            ProfilePatternDef {
                pattern: "married",
                key: "spouse",
                mode: "Between",
                suffix: Some("and"),
            },
            ProfilePatternDef {
                pattern: "known as",
                key: "alias",
                mode: "After",
                suffix: None,
            },
            ProfilePatternDef {
                pattern: "Count",
                key: "title",
                mode: "Before",
                suffix: None,
            },
            ProfilePatternDef {
                pattern: "Prince",
                key: "title",
                mode: "Before",
                suffix: None,
            },
            ProfilePatternDef {
                pattern: "Princess",
                key: "title",
                mode: "Before",
                suffix: None,
            },
            ProfilePatternDef {
                pattern: "General",
                key: "title",
                mode: "Before",
                suffix: None,
            },
            ProfilePatternDef {
                pattern: "Duke",
                key: "title",
                mode: "Before",
                suffix: None,
            },
        ]
    }

    fn discovery_markers(&self) -> &[&str] {
        &[
            "Prince", "Princess", "Count", "Countess", "General", "Mr.", "Mrs.", "Miss", "Dr.",
            "Sir", "Lord", "Lady", "Captain", "Colonel", "Major", "Doctor", "Father",
        ]
    }

    fn chapter_pattern(&self) -> &str {
        "BOOK|CHAPTER|EPILOGUE|PART"
    }

    fn strong_verbs(&self) -> Vec<String> {
        crate::dictionary::global()
            .english_strong_verbs()
            .iter()
            .cloned()
            .collect()
    }

    fn action_verbs(&self) -> Vec<String> {
        crate::dictionary::global()
            .english_action_verbs()
            .iter()
            .cloned()
            .collect()
    }

    fn hostile_verbs(&self) -> Vec<String> {
        crate::dictionary::global()
            .english_hostile_verbs()
            .iter()
            .cloned()
            .collect()
    }

    fn friendly_verbs(&self) -> Vec<String> {
        crate::dictionary::global()
            .english_friendly_verbs()
            .iter()
            .cloned()
            .collect()
    }
}
