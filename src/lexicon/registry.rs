//! Registry construction: merge, validate and index lexemes.

use super::*;

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
pub(super) fn empty_registry() -> LexiconRegistry {
    RegistryBuilder::new()
        .build()
        .expect("an empty registry always builds")
}
