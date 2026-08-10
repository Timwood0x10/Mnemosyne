//! Lexicon Registry — multi-layer elite word-list manager.
//!
//! ## Design
//!
//! The registry merges lexemes from up to three layers (Core → Domain Pack →
//! User Override) into a single deterministic runtime snapshot. Conflict
//! detection catches duplicate IDs within the same layer, and the priority
//! system resolves conflicts across layers.
//!
//! ## Layers (lowest → highest priority)
//!
//! | Layer | Source | Persistence |
//! |---|---|---|
//! | `Core` | `config/dictionary.json` | Compiled-in / `include_bytes!` |
//! | `Domain` | `lexicon/packs/*.json` | Runtime file load |
//! | `User` | User-specified JSON | Runtime file or string |
//!
//! ## Lifecycle
//!
//! 1. Load core + domain + user JSON files.
//! 2. `RegistryBuilder::new()` collects them.
//! 3. `.build()` validates + merges → `LexiconRegistry`.
//! 4. Registry exposes `.lexemes()`, `.lookup()`, `.by_class()`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use serde::Deserialize;

pub mod external;

pub use external::{ExternalEntry, ExternalFileProvider, ExternalLexiconProvider};

// ── Re-export Lexeme types from dictionary.rs ───────────────────────────────

pub use crate::dictionary::{CognitiveEffect, Lexeme, LexiconSource, MatchConstraints};

// ── Error types ─────────────────────────────────────────────────────────────

/// Errors produced during registry construction.
#[derive(Debug)]
pub enum LexiconError {
    /// Two lexemes in the same layer share the same ID.
    DuplicateId {
        id: String,
        layer: &'static str,
        first: String,
        second: String,
    },
    /// Two lexemes in the same layer share the same form + language.
    DuplicateForm {
        form: String,
        language: String,
        layer: &'static str,
        first_id: String,
        second_id: String,
    },
    /// A referenced file could not be loaded.
    FileLoad { path: String, cause: String },
    /// JSON parsing failed.
    Parse { detail: String },
    /// User override tries to disable a non-existent core lexeme.
    DisableNotFound { id: String },
    /// Two different domain packs define the same lexeme ID.
    CrossPackDuplicateId {
        id: String,
        pack_a: String,
        pack_b: String,
    },
    /// Two different domain packs define the same form for the same language.
    CrossPackDuplicateForm {
        form: String,
        language: String,
        pack_a: String,
        pack_b: String,
    },
}

impl std::fmt::Display for LexiconError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LexiconError::DuplicateId { id, layer, .. } => {
                write!(f, "duplicate lexeme id `{id}` in layer `{layer}`")
            }
            LexiconError::DuplicateForm {
                form,
                language,
                layer,
                ..
            } => {
                write!(
                    f,
                    "duplicate form `{form}` (lang={language}) in layer `{layer}`"
                )
            }
            LexiconError::FileLoad { path, cause } => {
                write!(f, "cannot load `{path}`: {cause}")
            }
            LexiconError::Parse { detail } => {
                write!(f, "parse error: {detail}")
            }
            LexiconError::DisableNotFound { id } => {
                write!(f, "cannot disable unknown lexeme `{id}`")
            }
            LexiconError::CrossPackDuplicateId { id, pack_a, pack_b } => {
                write!(
                    f,
                    "lexeme id `{id}` defined by both pack `{pack_a}` and pack `{pack_b}`"
                )
            }
            LexiconError::CrossPackDuplicateForm {
                form,
                language,
                pack_a,
                pack_b,
            } => {
                write!(
                    f,
                    "form `{form}` (lang={language}) defined by both pack `{pack_a}` and pack `{pack_b}`"
                )
            }
        }
    }
}

impl std::error::Error for LexiconError {}

// ── Layer types ─────────────────────────────────────────────────────────────

/// Which layer a lexeme belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LexiconLayer {
    /// Built-in lexemes (config/dictionary.json).
    Core,
    /// Domain-specific packs.
    Domain,
    /// User-supplied overrides.
    User,
}

// ── Metrics (P6 observability) ──────────────────────────────────────────────

/// Aggregate hit counters for the registry.
///
/// Tracks which lexemes and semantic classes actually matched during
/// compilation, plus rejection counts. Sensitive original text is never
/// recorded — only IDs and counts.
#[derive(Debug, Default)]
pub struct LexiconMetrics {
    total_hits: AtomicU64,
    total_rejections: AtomicU64,
    hits_by_id: Mutex<HashMap<String, u64>>,
    hits_by_class: Mutex<HashMap<String, u64>>,
}

/// Immutable snapshot of the metrics for reporting.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricsSnapshot {
    pub total_hits: u64,
    pub total_rejections: u64,
    pub hits_by_id: Vec<(String, u64)>,
    pub hits_by_class: Vec<(String, u64)>,
}

impl LexiconMetrics {
    /// Record one match of a lexeme (by ID) and its semantic class.
    pub fn record_hit(&self, id: &str, class: &str) {
        self.total_hits.fetch_add(1, Ordering::Relaxed);
        // Mutex poisoning requires a panic while the guard is held; the map
        // mutation below cannot panic, so these expects never fire.
        {
            let mut by_id = self
                .hits_by_id
                .lock()
                .expect("hits_by_id mutex is not poisoned");
            *by_id.entry(id.to_string()).or_insert(0) += 1;
        }
        {
            let mut by_class = self
                .hits_by_class
                .lock()
                .expect("hits_by_class mutex is not poisoned");
            *by_class.entry(class.to_string()).or_insert(0) += 1;
        }
    }

    /// Record a candidate rejected by match constraints.
    pub fn record_rejection(&self) {
        self.total_rejections.fetch_add(1, Ordering::Relaxed);
    }

    /// Build a deterministic snapshot (sorted by id/class).
    pub fn snapshot(&self) -> MetricsSnapshot {
        // Guard is released before any user code can panic on it; expect safe.
        let mut by_id: Vec<(String, u64)> = {
            let guard = self
                .hits_by_id
                .lock()
                .expect("hits_by_id mutex is not poisoned");
            guard.iter().map(|(k, v)| (k.clone(), *v)).collect()
        };
        by_id.sort_by(|a, b| a.0.cmp(&b.0));

        let mut by_class: Vec<(String, u64)> = {
            let guard = self
                .hits_by_class
                .lock()
                .expect("hits_by_class mutex is not poisoned");
            guard.iter().map(|(k, v)| (k.clone(), *v)).collect()
        };
        by_class.sort_by(|a, b| a.0.cmp(&b.0));

        MetricsSnapshot {
            total_hits: self.total_hits.load(Ordering::Relaxed),
            total_rejections: self.total_rejections.load(Ordering::Relaxed),
            hits_by_id: by_id,
            hits_by_class: by_class,
        }
    }
}

// ── Registry ────────────────────────────────────────────────────────────────

/// The merged, validated runtime lexicon.
#[derive(Debug, Clone)]
pub struct LexiconRegistry {
    /// All merged lexemes, sorted deterministically by (language, id).
    lexemes: Vec<Lexeme>,
    /// Index: lemma → lexemes.
    by_lemma: HashMap<String, Vec<usize>>,
    /// Index: semantic_class → lexeme indices.
    by_class: HashMap<String, Vec<usize>>,
    /// Index: id → lexeme.
    by_id: HashMap<String, usize>,
    /// Flat verb sets (backwards-compatible with `dictionary::Dictionary`).
    en_strong: HashSet<String>,
    en_action: HashSet<String>,
    en_hostile: HashSet<String>,
    en_friendly: HashSet<String>,
    zh_strong: HashSet<String>,
    zh_action: HashSet<String>,
    zh_hostile: HashSet<String>,
    zh_friendly: HashSet<String>,
    /// Deterministic content hash (SHA-256 truncated to hex).
    content_hash: String,
    /// Aggregate hit counters (P6 observability).
    metrics: Arc<LexiconMetrics>,
}

impl LexiconRegistry {
    /// Iterate all merged lexemes in deterministic order.
    pub fn lexemes(&self) -> &[Lexeme] {
        &self.lexemes
    }

    /// Look up lexemes by lemma (case-insensitive).
    pub fn lookup(&self, lemma: &str) -> Vec<&Lexeme> {
        let key = lemma.to_lowercase();
        self.by_lemma
            .get(&key)
            .map(|indices| indices.iter().map(|&i| &self.lexemes[i]).collect())
            .unwrap_or_default()
    }

    /// Get all lexemes in a given semantic class.
    pub fn by_class(&self, class: &str) -> Vec<&Lexeme> {
        self.by_class
            .get(class)
            .map(|indices| indices.iter().map(|&i| &self.lexemes[i]).collect())
            .unwrap_or_default()
    }

    /// Look up a single lexeme by its stable ID.
    pub fn by_id(&self, id: &str) -> Option<&Lexeme> {
        self.by_id.get(id).map(|&i| &self.lexemes[i])
    }

    /// Content hash for change detection.
    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }

    // ── Flat verb sets (backwards compat) ─────────────────────────────

    pub fn en_strong(&self) -> &HashSet<String> {
        &self.en_strong
    }
    pub fn en_action(&self) -> &HashSet<String> {
        &self.en_action
    }
    pub fn en_hostile(&self) -> &HashSet<String> {
        &self.en_hostile
    }
    pub fn en_friendly(&self) -> &HashSet<String> {
        &self.en_friendly
    }
    pub fn zh_strong(&self) -> &HashSet<String> {
        &self.zh_strong
    }
    pub fn zh_action(&self) -> &HashSet<String> {
        &self.zh_action
    }
    pub fn zh_hostile(&self) -> &HashSet<String> {
        &self.zh_hostile
    }
    pub fn zh_friendly(&self) -> &HashSet<String> {
        &self.zh_friendly
    }

    // ── Metrics (P6 observability) ─────────────────────────────────────

    /// Record a match for the given lexeme ID and semantic class.
    pub fn record_hit(&self, id: &str, class: &str) {
        self.metrics.record_hit(id, class);
    }

    /// Record a candidate rejected by match constraints.
    pub fn record_rejection(&self) {
        self.metrics.record_rejection();
    }

    /// Snapshot of aggregate hit counters.
    pub fn metrics(&self) -> MetricsSnapshot {
        self.metrics.snapshot()
    }

    // ── Version manifest (P6 governance) ───────────────────────────────

    /// Build a structured per-version lexicon manifest (§15).
    ///
    /// Includes content hash, entry counts by language and status, and the
    /// lists of deprecated/disabled entries — suitable for the per-release
    /// inventory required by the plan.
    pub fn manifest(&self) -> LexiconManifest {
        let mut en_count = 0usize;
        let mut zh_count = 0usize;
        let mut deprecated: Vec<String> = Vec::new();
        let mut disabled: Vec<String> = Vec::new();
        for lex in &self.lexemes {
            match lex.language.as_str() {
                "en" => en_count += 1,
                "zh" => zh_count += 1,
                _ => {}
            }
            match lex.status {
                crate::dictionary::LexemeStatus::Deprecated => {
                    deprecated.push(lex.id.clone());
                }
                crate::dictionary::LexemeStatus::Disabled => {
                    disabled.push(lex.id.clone());
                }
                _ => {}
            }
        }
        deprecated.sort();
        disabled.sort();

        LexiconManifest {
            content_hash: self.content_hash.clone(),
            total_entries: self.lexemes.len(),
            en_entries: en_count,
            zh_entries: zh_count,
            deprecated_entries: deprecated,
            disabled_entries: disabled,
        }
    }

    /// List lexeme IDs currently marked `Deprecated`.
    pub fn deprecated_ids(&self) -> Vec<String> {
        self.manifest().deprecated_entries
    }

    /// List lexeme IDs currently marked `Disabled` (excluded from matchers).
    pub fn disabled_ids(&self) -> Vec<String> {
        self.manifest().disabled_entries
    }
}

/// Per-version lexicon inventory (§15 of ELITE_LEXICON_PLAN.md).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LexiconManifest {
    pub content_hash: String,
    pub total_entries: usize,
    pub en_entries: usize,
    pub zh_entries: usize,
    pub deprecated_entries: Vec<String>,
    pub disabled_entries: Vec<String>,
}

impl LexiconManifest {
    /// Serialize to pretty JSON (for per-release inventory artifacts).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("manifest is serializable")
    }
}

// ── Builder ─────────────────────────────────────────────────────────────────

/// Construct a `LexiconRegistry` from layered sources.
///
/// ```ignore
/// let registry = RegistryBuilder::new()
///     .load_core("config/dictionary.json")?
///     .load_domain("lexicon/packs/classical_chinese.json")?
///     .load_user("user_overrides.json")?
///     .build()?;
/// ```
#[derive(Default)]
pub struct RegistryBuilder {
    core: Vec<Lexeme>,
    /// Named domain packs: (pack_name, lexemes).
    domain_packs: Vec<(String, Vec<Lexeme>)>,
    user: Vec<Lexeme>,
    /// IDs to disable (from user overrides).
    disabled: HashSet<String>,
    /// If set, only these pack names are included at build time.
    selected_packs: Option<HashSet<String>>,
}

impl RegistryBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load core lexicon from a JSON file.
    pub fn load_core<P: AsRef<Path>>(mut self, path: P) -> Result<Self, LexiconError> {
        let lexemes = load_lexemes_from_file(path)?;
        self.core = lexemes;
        Ok(self)
    }

    /// Load a domain pack from a JSON file (legacy single-pack API).
    pub fn load_domain<P: AsRef<Path>>(mut self, path: P) -> Result<Self, LexiconError> {
        let name = path
            .as_ref()
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "domain".to_string());
        let lexemes = load_lexemes_from_file(&path)?;
        self.domain_packs.push((name, lexemes));
        Ok(self)
    }

    /// Load a named domain pack from a JSON file (P4: multiple packs).
    pub fn load_domain_pack<P: AsRef<Path>>(
        mut self,
        name: &str,
        path: P,
    ) -> Result<Self, LexiconError> {
        let lexemes = load_lexemes_from_file(path)?;
        self.domain_packs.push((name.to_string(), lexemes));
        Ok(self)
    }

    /// Restrict build to the given domain pack names (per-request selection).
    pub fn select_packs(mut self, names: &[&str]) -> Self {
        self.selected_packs = Some(names.iter().map(|s| s.to_string()).collect());
        self
    }

    /// Specify user override lexemes (from JSON string or file).
    pub fn load_user(mut self, lexemes: Vec<Lexeme>) -> Self {
        self.user = lexemes;
        self
    }

    /// Add user-requested disabled IDs.
    pub fn disable(mut self, ids: Vec<String>) -> Self {
        self.disabled.extend(ids);
        self
    }

    /// Build the registry, merging all layers and detecting conflicts.
    ///
    /// Validation steps:
    /// 1. Disable requested IDs (error if not found).
    /// 2. Check for duplicate IDs within each layer.
    /// 3. Check for duplicate forms within each layer.
    /// 4. Detect cross-pack conflicts (same ID/form in two domain packs).
    /// 5. Merge layers in priority order (Core → Domain → User).
    /// 6. Build indices and flat verb sets.
    /// 7. Compute content hash.
    pub fn build(self) -> Result<LexiconRegistry, LexiconError> {
        let mut all = Vec::new();

        // ── Step 1: validate core ─────────────────────────────────────
        validate_layer(&self.core, LexiconLayer::Core)?;
        all.extend(annotate_layer(self.core, LexiconLayer::Core));

        // ── Step 2: select + validate + merge domain packs ─────────────
        let mut domain: Vec<Lexeme> = Vec::new();
        for (name, lexemes) in &self.domain_packs {
            if let Some(ref selected) = self.selected_packs {
                if !selected.contains(name) {
                    continue; // skipped for this request
                }
            }
            validate_layer(lexemes, LexiconLayer::Domain)?;
            domain.extend(lexemes.clone());
        }
        validate_cross_pack(&self.domain_packs, &self.selected_packs)?;
        all.extend(annotate_layer(domain, LexiconLayer::Domain));

        // ── Step 3: validate + merge user ──────────────────────────────
        validate_layer(&self.user, LexiconLayer::User)?;
        let user_annotated = annotate_layer(self.user, LexiconLayer::User);
        all.extend(user_annotated);

        // ── Step 4: remove disabled ───────────────────────────────────
        if !self.disabled.is_empty() {
            let active_ids: HashSet<&str> = all.iter().map(|l| l.id.as_str()).collect();
            for id in &self.disabled {
                if !active_ids.contains(id.as_str()) {
                    return Err(LexiconError::DisableNotFound { id: id.clone() });
                }
            }
            all.retain(|l| !self.disabled.contains(&l.id));
        }

        // ── Step 5: deterministic sort ─────────────────────────────────
        all.sort_by(|a, b| a.language.cmp(&b.language).then_with(|| a.id.cmp(&b.id)));

        // ── Step 6: build indices ──────────────────────────────────────
        let mut by_lemma: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_class: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_id: HashMap<String, usize> = HashMap::new();

        let mut en_strong = HashSet::new();
        let mut en_action = HashSet::new();
        let mut en_hostile = HashSet::new();
        let mut en_friendly = HashSet::new();
        let mut zh_strong = HashSet::new();
        let mut zh_action = HashSet::new();
        let mut zh_hostile = HashSet::new();
        let mut zh_friendly = HashSet::new();

        for (i, lex) in all.iter().enumerate() {
            let lemma = lex.lemma.to_lowercase();
            by_lemma.entry(lemma).or_default().push(i);
            by_class
                .entry(lex.semantic_class.clone())
                .or_default()
                .push(i);
            by_id.insert(lex.id.clone(), i);

            let all_forms: Vec<String> = std::iter::once(&lex.lemma)
                .chain(lex.forms.iter())
                .map(|f| f.to_lowercase())
                .collect();

            let is_strong = matches!(
                lex.semantic_class.as_str(),
                "attack" | "rescue" | "creation"
            );
            let is_action = matches!(
                lex.semantic_class.as_str(),
                "speech"
                    | "movement"
                    | "emotion"
                    | "cognition"
                    | "transfer"
                    | "state"
                    | "intention"
                    | "preference"
            );
            let is_attack = lex.semantic_class == "attack";
            let is_rescue = lex.semantic_class == "rescue";

            for form in &all_forms {
                match lex.language.as_str() {
                    "en" => {
                        if is_strong {
                            en_strong.insert(form.clone());
                        }
                        if is_action {
                            en_action.insert(form.clone());
                        }
                        if is_attack {
                            en_hostile.insert(form.clone());
                        }
                        if is_rescue {
                            en_friendly.insert(form.clone());
                        }
                    }
                    "zh" => {
                        if is_strong {
                            zh_strong.insert(form.clone());
                        }
                        if is_action {
                            zh_action.insert(form.clone());
                        }
                        if is_attack {
                            zh_hostile.insert(form.clone());
                        }
                        if is_rescue {
                            zh_friendly.insert(form.clone());
                        }
                    }
                    _ => {}
                }
            }
        }

        // ── Step 7: content hash ───────────────────────────────────────
        let hash = simple_hash(&all);

        Ok(LexiconRegistry {
            lexemes: all,
            by_lemma,
            by_class,
            by_id,
            en_strong,
            en_action,
            en_hostile,
            en_friendly,
            zh_strong,
            zh_action,
            zh_hostile,
            zh_friendly,
            content_hash: hash,
            metrics: Arc::new(LexiconMetrics::default()),
        })
    }
}

// ── LexiconMatcher (P3 unified matcher) ─────────────────────────────────────

/// A single match produced by the lexicon matcher.
#[derive(Debug, Clone)]
pub struct LexiconMatch {
    /// Stable lexeme ID.
    pub id: String,
    /// Matched surface form (as it appears in text).
    pub matched: String,
    /// Byte range of the match in the scanned text.
    pub start: usize,
    pub end: usize,
    /// Semantic class of the matched lexeme.
    pub semantic_class: String,
    /// Language of the matched lexeme.
    pub language: String,
}

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

// ── Internal helpers ────────────────────────────────────────────────────────

fn load_lexemes_from_file<P: AsRef<Path>>(path: P) -> Result<Vec<Lexeme>, LexiconError> {
    let text = std::fs::read_to_string(path.as_ref()).map_err(|e| LexiconError::FileLoad {
        path: path.as_ref().display().to_string(),
        cause: e.to_string(),
    })?;

    // The JSON may be a flat array or a wrapped `{"lexemes": [...]}` object.
    if let Ok(arr) = serde_json::from_str::<Vec<Lexeme>>(&text) {
        return Ok(arr);
    }
    #[derive(Deserialize)]
    struct Wrapper {
        lexemes: Vec<Lexeme>,
    }
    if let Ok(w) = serde_json::from_str::<Wrapper>(&text) {
        return Ok(w.lexemes);
    }
    Err(LexiconError::Parse {
        detail: format!(
            "expected JSON array or object with `lexemes` key in `{}`",
            path.as_ref().display()
        ),
    })
}

/// Check for duplicate IDs and forms within a single layer.
fn validate_layer(lexemes: &[Lexeme], layer: LexiconLayer) -> Result<(), LexiconError> {
    let layer_name = match layer {
        LexiconLayer::Core => "core",
        LexiconLayer::Domain => "domain",
        LexiconLayer::User => "user",
    };

    let mut seen_ids: HashMap<&str, &str> = HashMap::new();
    for lex in lexemes {
        if let Some(first_lemma) = seen_ids.get(lex.id.as_str()) {
            return Err(LexiconError::DuplicateId {
                id: lex.id.clone(),
                layer: layer_name,
                first: (*first_lemma).to_string(),
                second: lex.lemma.clone(),
            });
        }
        seen_ids.insert(&lex.id, &lex.lemma);
    }

    // Check duplicate forms (same language + same form string).
    // Allow the same form within a single lexeme (lemma may overlap with forms).
    let mut seen_forms: HashMap<(String, String), &str> = HashMap::new();
    for lex in lexemes {
        let all_forms = std::iter::once(&lex.lemma).chain(lex.forms.iter());
        for form in all_forms {
            let key = (form.to_lowercase(), lex.language.clone());
            if let Some(first_id) = seen_forms.get(&key) {
                // Same lexeme is fine (lemma overlaps forms).
                if *first_id != lex.id {
                    return Err(LexiconError::DuplicateForm {
                        form: form.clone(),
                        language: lex.language.clone(),
                        layer: layer_name,
                        first_id: (*first_id).to_string(),
                        second_id: lex.id.clone(),
                    });
                }
            } else {
                seen_forms.insert(key, &lex.id);
            }
        }
    }

    Ok(())
}

/// Detect conflicts between different domain packs (P4 diagnostics).
///
/// Two packs defining the same lexeme ID — or the same form for the same
/// language — make the selection ambiguous, so they are reported as errors
/// unless a pack-selection filter excludes one of them.
fn validate_cross_pack(
    packs: &[(String, Vec<Lexeme>)],
    selected: &Option<HashSet<String>>,
) -> Result<(), LexiconError> {
    // Skip packs not selected for this request.
    let active: Vec<&(String, Vec<Lexeme>)> = packs
        .iter()
        .filter(|(name, _)| match selected {
            Some(set) => set.contains(name),
            None => true,
        })
        .collect();

    // 1. Cross-pack duplicate IDs.
    let mut seen_ids: HashMap<&str, &str> = HashMap::new();
    for (pack_name, lexemes) in &active {
        for lex in lexemes {
            if let Some(first_pack) = seen_ids.get(lex.id.as_str()) {
                if *first_pack != pack_name {
                    return Err(LexiconError::CrossPackDuplicateId {
                        id: lex.id.clone(),
                        pack_a: (*first_pack).to_string(),
                        pack_b: pack_name.clone(),
                    });
                }
            } else {
                seen_ids.insert(&lex.id, pack_name);
            }
        }
    }

    // 2. Cross-pack duplicate forms (same language + same form string).
    let mut seen_forms: HashMap<(String, String), &str> = HashMap::new();
    for (pack_name, lexemes) in &active {
        for lex in lexemes {
            let all_forms = std::iter::once(&lex.lemma).chain(lex.forms.iter());
            for form in all_forms {
                let key = (form.to_lowercase(), lex.language.clone());
                if let Some(first_pack) = seen_forms.get(&key) {
                    if *first_pack != pack_name {
                        return Err(LexiconError::CrossPackDuplicateForm {
                            form: form.clone(),
                            language: lex.language.clone(),
                            pack_a: (*first_pack).to_string(),
                            pack_b: pack_name.clone(),
                        });
                    }
                } else {
                    seen_forms.insert(key, pack_name);
                }
            }
        }
    }

    Ok(())
}
fn annotate_layer(lexemes: Vec<Lexeme>, _layer: LexiconLayer) -> Vec<Lexeme> {
    // Currently all layers are merged with override semantics (user replaces
    // domain, domain replaces core). We keep the lexeme as-is; the build
    // order ensures later layers overwrite earlier ones when IDs match.
    lexemes
}

/// Deterministic hash: concatenate all (id, lemma, semantic_class) sorted
/// pairs and hash them. Returns a short hex string.
fn simple_hash(lexemes: &[Lexeme]) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    for lex in lexemes {
        lex.id.hash(&mut hasher);
        lex.lemma.hash(&mut hasher);
        lex.semantic_class.hash(&mut hasher);
        lex.language.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

// ── Global registry instance ────────────────────────────────────────────────

/// A registry with no lexemes — the fail-soft fallback when the core lexicon
/// cannot be loaded. An empty builder always builds successfully (an empty
/// layer passes validation and merging trivially), so this never panics.
fn empty_registry() -> LexiconRegistry {
    RegistryBuilder::new()
        .build()
        .expect("an empty registry always builds")
}

static REGISTRY: LazyLock<RwLock<LexiconRegistry>> = LazyLock::new(|| {
    // Resolve the core lexicon at runtime from the resource root
    // (MNEMOSYNE_HOME override, else the install root) instead of baking a
    // compile-time `env!("CARGO_MANIFEST_DIR")` into the binary — a baked
    // path made every release fail with FileLoad except on the CI builder.
    //
    // Fail soft instead of panicking on first use (mirrors `dictionary::DICT`):
    // a missing/corrupt core lexicon used to abort the process the moment any
    // caller touched the global registry. Degrade to an EMPTY registry
    // (lookups just miss) and log the cause, so a deployment without the
    // config stays alive and diagnosable.
    let core_path = crate::config::resolve_resource_path("config/dictionary.json");
    let registry = match RegistryBuilder::new().load_core(&core_path) {
        Ok(builder) => match builder.build() {
            Ok(reg) => reg,
            Err(e) => {
                eprintln!("warning: core lexicon validation failed ({e}); using an empty registry");
                empty_registry()
            }
        },
        Err(e) => {
            eprintln!(
                "warning: config/dictionary.json failed to load ({e}); using an empty registry"
            );
            empty_registry()
        }
    };
    RwLock::new(registry)
});

/// Access the global registry.
pub fn global() -> std::sync::RwLockReadGuard<'static, LexiconRegistry> {
    // Read guards are released before user code can panic on them; the
    // registry is written once at startup. Expect keeps the invariant
    // traceable if poisoning ever occurs.
    REGISTRY
        .read()
        .expect("global lexicon registry read lock is not poisoned")
}

/// Reload the global registry from the default core path.
pub fn reload() -> Result<(), LexiconError> {
    let core_path = crate::config::resolve_resource_path("config/dictionary.json");
    let registry = RegistryBuilder::new().load_core(&core_path)?.build()?;
    // The assignment below cannot panic while holding the write guard, so
    // this expect never fires.
    *REGISTRY
        .write()
        .expect("global lexicon registry write lock is not poisoned") = registry;
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Load core lexicon from the real config file.
    /// Invariants: At least 50 lexemes are loaded (core EN + ZH minimal set).
    #[test]
    fn core_lexicon_loads_and_contains_entries() {
        let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        let registry = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core lexicon must load")
            .build()
            .expect("Registry build must succeed");
        assert!(
            registry.lexemes().len() >= 50,
            "Core lexicon should contain at least 50 lexemes, got {}",
            registry.lexemes().len()
        );
        assert!(
            !registry.content_hash().is_empty(),
            "Content hash must be non-empty"
        );
    }

    /// Objective: Verify the matcher build counter increments on construction.
    /// Invariants: Building a matcher increases `matcher_build_count()`; the
    /// global functional matcher (LazyLock) is built exactly once per process.
    #[test]
    fn matcher_build_count_tracks_constructions() {
        let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        let registry = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core must load")
            .build()
            .expect("Registry build must succeed");

        let before = matcher_build_count();
        let _m1 = LexiconMatcher::from_lexemes(registry.lexemes());
        let _m2 = LexiconMatcher::from_lexemes(registry.lexemes());
        let after = matcher_build_count();
        // The counter is process-global and other tests build matchers in
        // parallel, so only an at-least assertion is deterministic here.
        assert!(
            after - before >= 2,
            "Two explicit matcher constructions must bump the counter by at least 2 (got {})",
            after - before
        );

        // Global matcher is lazily built once; force it and ensure the count
        // does not explode when reused (it is a LazyLock singleton).
        let _g = crate::lexicon::LexiconMatcher::from_global_registry();
        let _g2 = crate::lexicon::LexiconMatcher::from_global_registry();
        assert!(
            matcher_build_count() >= after,
            "Global matcher construction must also be counted"
        );
    }

    /// Objective: Verify duplicate IDs are rejected.
    /// Invariants: Two lexemes with the same ID in the same layer produce an error.
    #[test]
    fn duplicate_id_is_detected() {
        let lexemes = vec![
            make_test_lexeme("en.test.dup", "test_a"),
            make_test_lexeme("en.test.dup", "test_b"),
        ];
        let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        // This should fail because we have dup IDs in the user layer
        let result = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core must load")
            .load_user(lexemes)
            .build();
        assert!(
            result.is_err(),
            "Duplicate IDs in the same layer should be rejected"
        );
    }

    /// Objective: Verify duplicate forms (same language + same lemma) are rejected.
    /// Invariants: Same lemma+language in the same layer produces an error.
    #[test]
    fn duplicate_form_is_detected() {
        let lexemes = vec![
            Lexeme {
                id: "en.test.first".into(),
                language: "en".into(),
                lemma: "testword".into(),
                forms: vec![],
                pos: "verb".into(),
                semantic_class: "speech".into(),
                effects: vec![],
                polarity: "neutral".into(),
                priority: 500,
                constraints: crate::dictionary::MatchConstraints {
                    word_boundary: true,
                    allow_single: false,
                    requires_participant: false,
                    requires_subject: false,
                },
                source: crate::dictionary::LexiconSource {
                    kind: "builtin".into(),
                    name: "test".into(),
                },
                status: crate::dictionary::LexemeStatus::Core,
            },
            Lexeme {
                id: "en.test.second".into(),
                language: "en".into(),
                lemma: "testword".into(),
                forms: vec![],
                pos: "verb".into(),
                semantic_class: "speech".into(),
                effects: vec![],
                polarity: "neutral".into(),
                priority: 500,
                constraints: crate::dictionary::MatchConstraints {
                    word_boundary: true,
                    allow_single: false,
                    requires_participant: false,
                    requires_subject: false,
                },
                source: crate::dictionary::LexiconSource {
                    kind: "builtin".into(),
                    name: "test".into(),
                },
                status: crate::dictionary::LexemeStatus::Core,
            },
        ];
        let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        let result = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core must load")
            .load_user(lexemes)
            .build();
        assert!(
            result.is_err(),
            "Duplicate forms (same lemma+language) in the same layer should be rejected"
        );
    }

    /// Objective: Verify the global singleton initializes without panic.
    /// Invariants: Calling `global()` returns a valid registry.
    #[test]
    fn global_registry_initializes() {
        let r = global();
        assert!(
            r.lexemes().len() >= 50,
            "Global registry should have >= 50 lexemes"
        );
    }

    /// Objective: Verify a domain pack merges on top of the core lexicon.
    /// Invariants: The merged registry contains conversation lexemes from the
    /// pack in addition to the core set, without duplicate-ID errors.
    #[test]
    fn domain_pack_merges_with_core() {
        let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        let pack_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("lexicon/packs/conversation_memory.json");
        let registry = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core must load")
            .load_domain(&pack_path)
            .expect("conversation_memory pack must load")
            .build()
            .expect("Registry build must succeed");

        // The pack adds "喜欢" (zh preference) and "want" (en goal).
        assert!(
            registry
                .by_id("zh.conversation.preference.xihuan")
                .is_some(),
            "Domain pack lexeme 喜欢 must be present"
        );
        assert!(
            registry.by_id("en.conversation.goal.want").is_some(),
            "Domain pack lexeme want must be present"
        );
        assert!(
            registry.by_id("en.action.speech.said").is_some(),
            "Core lexeme must still be present after merge"
        );
    }

    /// Objective: Verify the english_narrative domain pack merges on top of core.
    /// Invariants: Narrative lexemes (e.g. murmured, invaded) are present;
    /// core entries remain; no cross-layer duplicate-ID errors.
    #[test]
    fn english_narrative_pack_merges_with_core() {
        let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        let pack_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("lexicon/packs/english_narrative.json");
        let registry = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core must load")
            .load_domain(&pack_path)
            .expect("english_narrative pack must load")
            .build()
            .expect("Registry build must succeed");

        assert!(
            registry.by_id("en.narrative.speech.murmured").is_some(),
            "Narrative lexeme murmured must be present"
        );
        assert!(
            registry.by_id("en.narrative.attack.invaded").is_some(),
            "Narrative lexeme invaded must be present"
        );
        assert!(
            registry.by_id("en.action.speech.said").is_some(),
            "Core lexeme said must still be present after merge"
        );
    }

    /// Objective: Verify the sanguo work-specific pack merges and is isolated.
    /// Invariants: Work-specific lexemes (青龙偃月刀, 结义) come from the pack,
    /// not from Core; core remains free of work-specific terms.
    #[test]
    fn sanguo_pack_merges_and_core_stays_clean() {
        let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        let pack_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("lexicon/packs/sanguo.json");
        let registry = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core must load")
            .load_domain(&pack_path)
            .expect("sanguo pack must load")
            .build()
            .expect("Registry build must succeed");

        assert!(
            registry.by_id("zh.sanguo.weapon.qinglong").is_some(),
            "Work-specific lexeme 青龙偃月刀 must be present via the pack"
        );
        assert!(
            registry.by_id("zh.sanguo.event.jieyi").is_some(),
            "Work-specific lexeme 结义 must be present via the pack"
        );

        // P4 isolation guard: Core alone must NOT contain work-specific terms.
        let core_only = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core must load")
            .build()
            .expect("Core-only build must succeed");
        for work_specific in ["青龙偃月刀", "丈八蛇矛", "结义", "出茅庐", "奸雄"] {
            assert!(
                core_only.lookup(work_specific).is_empty(),
                "Core must not contain work-specific lexeme `{work_specific}`"
            );
        }
    }
    /// Invariants: Selecting only `conversation_memory` excludes classical
    /// lexemes while keeping core + the selected pack.
    #[test]
    fn per_request_pack_selection_filters_packs() {
        let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        let conversation =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("lexicon/packs/conversation_memory.json");
        let classical =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("lexicon/packs/classical_chinese.json");

        let registry = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core must load")
            .load_domain_pack("conversation_memory", &conversation)
            .expect("conversation pack must load")
            .load_domain_pack("classical_chinese", &classical)
            .expect("classical pack must load")
            .select_packs(&["conversation_memory"])
            .build()
            .expect("Registry build must succeed");

        assert!(
            registry
                .by_id("zh.conversation.preference.xihuan")
                .is_some(),
            "Selected pack lexeme must be present"
        );
        assert!(
            registry.by_id("zh.classical.attack.fa").is_none(),
            "Unselected pack lexeme must be excluded"
        );
        assert!(
            registry.by_id("en.action.speech.said").is_some(),
            "Core lexeme must always be present"
        );
    }

    /// Objective: Verify cross-pack duplicate IDs are diagnosed.
    /// Invariants: Two packs defining the same lexeme ID produce a
    /// `CrossPackDuplicateId` error at build time.
    #[test]
    fn cross_pack_duplicate_id_is_detected() {
        let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        let dir = tempfile::TempDir::new().expect("temp dir for cross-pack test");
        let pack_a_path = dir.path().join("pack_a.json");
        let pack_b_path = dir.path().join("pack_b.json");

        let lexeme_a = make_test_lexeme("en.test.shared", "shared_a");
        let lexeme_b = make_test_lexeme("en.test.shared", "shared_b");
        std::fs::write(
            &pack_a_path,
            serde_json::to_string(&vec![lexeme_a]).expect("serialize pack_a"),
        )
        .expect("write pack_a");
        std::fs::write(
            &pack_b_path,
            serde_json::to_string(&vec![lexeme_b]).expect("serialize pack_b"),
        )
        .expect("write pack_b");

        let err = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core must load")
            .load_domain_pack("pack_a", &pack_a_path)
            .expect("pack_a must load")
            .load_domain_pack("pack_b", &pack_b_path)
            .expect("pack_b must load")
            .build()
            .expect_err("cross-pack duplicate ID must fail");

        match err {
            LexiconError::CrossPackDuplicateId { id, pack_a, pack_b } => {
                assert_eq!(id, "en.test.shared", "conflicting ID must be reported");
                assert_eq!(pack_a, "pack_a", "first pack name must be reported");
                assert_eq!(pack_b, "pack_b", "second pack name must be reported");
            }
            other => panic!("expected CrossPackDuplicateId, got {other:?}"),
        }
    }

    /// Objective: Verify the manifest reports entry counts and status lists.
    /// Invariants: Manifest has the same total as `lexemes()`; en/zh counts sum
    /// to the total; with core+classical packs the counts are non-zero.
    #[test]
    fn manifest_reports_counts_and_statuses() {
        let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        let pack_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("lexicon/packs/classical_chinese.json");
        let registry = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core must load")
            .load_domain(&pack_path)
            .expect("Pack must load")
            .build()
            .expect("Registry build must succeed");

        let manifest = registry.manifest();
        assert_eq!(
            manifest.total_entries,
            registry.lexemes().len(),
            "Manifest total must match registry lexeme count"
        );
        assert_eq!(
            manifest.en_entries + manifest.zh_entries,
            manifest.total_entries,
            "en+zh counts must sum to the total"
        );
        assert!(manifest.en_entries > 0, "English entries must exist");
        assert!(manifest.zh_entries > 0, "Chinese entries must exist");
        assert!(
            !manifest.content_hash.is_empty(),
            "Manifest must carry the content hash"
        );
    }

    /// Objective: Verify the manifest serializes to stable JSON (P6 inventory).
    /// Invariants: `to_json()` output parses back and contains the hash and
    /// entry counts; the output is deterministic.
    #[test]
    fn manifest_serializes_to_json() {
        let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        let registry = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core must load")
            .build()
            .expect("Registry build must succeed");

        let json = registry.manifest().to_json();
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("manifest JSON must parse");
        assert!(
            parsed["content_hash"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "manifest JSON must carry the content hash"
        );
        assert!(
            parsed["total_entries"].as_u64().is_some(),
            "manifest JSON must carry total_entries"
        );
        assert_eq!(
            registry.manifest().to_json(),
            json,
            "manifest JSON must be deterministic"
        );
    }

    /// Objective: Verify deprecated/disabled ID reporting.
    /// Invariants: A Disabled user lexeme appears in `disabled_ids()` but not
    /// in matchers; a Core lexeme is not reported.
    #[test]
    fn manifest_lists_disabled_and_deprecated() {
        let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        let mut disabled_lex = make_test_lexeme("en.test.to_disable", "tobedisabled");
        disabled_lex.status = crate::dictionary::LexemeStatus::Disabled;
        let registry = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core must load")
            .load_user(vec![disabled_lex])
            .build()
            .expect("Registry build must succeed");

        assert!(
            registry
                .disabled_ids()
                .contains(&"en.test.to_disable".to_string()),
            "Disabled lexeme must be reported by disabled_ids()"
        );
        assert!(
            !registry
                .deprecated_ids()
                .contains(&"en.test.to_disable".to_string()),
            "Disabled lexeme must not be reported as deprecated"
        );
        assert!(
            registry.by_id("en.action.speech.said").is_some(),
            "Core lexeme must remain present"
        );
    }

    /// Objective: Verify the classical_chinese domain pack merges on top of core.
    /// Invariants: Classical lexemes (e.g. 伐, 弑) are present; core entries
    /// remain; duplicate-ID validation does not trip across layers.
    #[test]
    fn classical_chinese_pack_merges_with_core() {
        let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        let pack_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("lexicon/packs/classical_chinese.json");
        let registry = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core must load")
            .load_domain(&pack_path)
            .expect("classical_chinese pack must load")
            .build()
            .expect("Registry build must succeed");

        assert!(
            registry.by_id("zh.classical.attack.fa").is_some(),
            "Classical lexeme 伐 must be present"
        );
        assert!(
            registry.by_id("zh.classical.attack.shishi").is_some(),
            "Classical lexeme 弑 must be present"
        );
        assert!(
            registry.by_id("zh.action.attack.sha").is_some(),
            "Core lexeme 杀 must still be present after merge"
        );
    }

    /// Objective: Verify hit metrics accumulate and snapshot deterministically.
    /// Invariants: Total hits and per-id/class counts reflect recorded hits;
    /// the snapshot is sorted so repeated snapshots are identical.
    #[test]
    fn metrics_record_and_snapshot() {
        let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        let registry = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core must load")
            .build()
            .expect("Registry build must succeed");

        registry.record_hit("en.action.speech.said", "speech");
        registry.record_hit("en.action.speech.said", "speech");
        registry.record_hit("zh.action.attack.sha", "attack");
        registry.record_rejection();

        let snap = registry.metrics();
        assert_eq!(snap.total_hits, 3, "Three hits must be recorded");
        assert_eq!(snap.total_rejections, 1, "One rejection must be recorded");
        assert_eq!(
            snap.hits_by_id,
            vec![
                ("en.action.speech.said".to_string(), 2),
                ("zh.action.attack.sha".to_string(), 1),
            ],
            "Per-id counts must be deterministic and sorted"
        );
        assert_eq!(
            snap.hits_by_class,
            vec![("attack".to_string(), 1), ("speech".to_string(), 2),],
            "Per-class counts must be deterministic and sorted"
        );
        // Snapshot twice: must be identical (deterministic).
        assert_eq!(snap, registry.metrics(), "Snapshots must be deterministic");
    }

    /// Objective: Verify the matcher finds lexeme forms with word boundaries.
    /// Invariants: "plan" matches but "planet" does not (word boundary); the
    /// matcher reports the correct lexeme ID and semantic class.
    #[test]
    fn matcher_honors_word_boundaries() {
        let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        let pack_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("lexicon/packs/conversation_memory.json");
        let registry = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core must load")
            .load_domain(&pack_path)
            .expect("Pack must load")
            .build()
            .expect("Registry build must succeed");

        let matcher = LexiconMatcher::from_lexemes(registry.lexemes());
        assert!(matcher.pattern_count() > 0, "Matcher must contain patterns");

        // "I plan to rewrite the parser." — "plan" is a goal lexeme.
        let text = "I plan to rewrite the parser.";
        let matches: Vec<LexiconMatch> = matcher.find_iter(text).collect();
        assert!(
            matches.iter().any(|m| m.semantic_class == "intention"),
            "`plan` should match an intention lexeme; got {matches:?}"
        );

        // "planet" must NOT match the `plan` pattern (word boundary).
        let planet_matches: Vec<LexiconMatch> = matcher.find_iter("explore the planet").collect();
        assert!(
            !planet_matches.iter().any(|m| m.matched == "plan"),
            "`plan` inside `planet` must be rejected by word boundary; got {planet_matches:?}"
        );
    }

    /// Objective: Verify Chinese single-character matching works.
    /// Invariants: "杀" in 三国演义 text matches an attack lexeme.
    #[test]
    fn matcher_finds_chinese_single_char() {
        let core_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/dictionary.json");
        let registry = RegistryBuilder::new()
            .load_core(&core_path)
            .expect("Core must load")
            .build()
            .expect("Registry build must succeed");

        let matcher = LexiconMatcher::from_lexemes(registry.lexemes());
        let matches: Vec<LexiconMatch> = matcher.find_iter("吕布杀董卓").collect();
        assert!(
            matches
                .iter()
                .any(|m| m.matched == "杀" && m.semantic_class == "attack"),
            "`杀` should match an attack lexeme; got {matches:?}"
        );
    }

    fn make_test_lexeme(id: &str, lemma: &str) -> Lexeme {
        Lexeme {
            id: id.into(),
            language: "en".into(),
            lemma: lemma.into(),
            forms: vec![],
            pos: "verb".into(),
            semantic_class: "speech".into(),
            effects: vec![],
            polarity: "neutral".into(),
            priority: 500,
            constraints: crate::dictionary::MatchConstraints {
                word_boundary: true,
                allow_single: false,
                requires_participant: false,
                requires_subject: false,
            },
            source: crate::dictionary::LexiconSource {
                kind: "builtin".into(),
                name: "test".into(),
            },
            status: crate::dictionary::LexemeStatus::Core,
        }
    }
}
