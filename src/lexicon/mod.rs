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
//! | `User` | `lexicon/user.json` | Runtime file |
//!
//! ## Lifecycle
//!
//! 1. Load core + domain + user JSON files.
//! 2. `RegistryBuilder::new()` collects them.
//! 3. `.build()` validates + merges → `LexiconRegistry`.
//! 4. Registry exposes `.lexemes()`, `.lookup()`, `.by_class()`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
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

mod matcher;
mod registry;

pub use matcher::{LexiconMatcher, matcher_build_count};
pub use registry::{LexiconManifest, LexiconRegistry, RegistryBuilder};

use registry::empty_registry;

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

/// User-override lexicon, applied on top of core + domain packs.
const USER_LEXICON_FILE: &str = "lexicon/user.json";

/// The `lexicon/packs/*.json` overlay files under `root`, sorted by name.
///
/// Sorted so the merged registry is deterministic regardless of directory
/// iteration order. A missing directory is not an error (packs are optional).
fn list_domain_pack_paths(root: &Path) -> Vec<PathBuf> {
    let dir = root.join("lexicon/packs");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        .collect();
    paths.sort();
    paths
}

/// Parse a user-override lexicon file (a flat array or `{"lexemes": [...]}`).
fn load_user_lexemes(path: &Path) -> Result<Vec<Lexeme>, LexiconError> {
    let text = std::fs::read_to_string(path).map_err(|e| LexiconError::FileLoad {
        path: path.display().to_string(),
        cause: e.to_string(),
    })?;
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
            path.display()
        ),
    })
}

/// Build the fully layered registry under `root`, excluding `disabled` ids.
///
/// Layer order (lowest → highest priority) matches the module docs:
/// `config/dictionary.json` → `lexicon/packs/*.json` → `lexicon/user.json`.
fn build_layers(root: &Path, disabled: &[String]) -> Result<LexiconRegistry, LexiconError> {
    let core_path = root.join("config/dictionary.json");
    let mut builder = RegistryBuilder::new().load_core(&core_path)?;

    for pack_path in list_domain_pack_paths(root) {
        let name = pack_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "domain".to_string());
        // Probe-parse first: an optional pack that is broken must be skipped
        // (and logged) rather than take the whole lexicon down, and the probe
        // lets us keep the partially built `builder` — `load_domain_pack`
        // consumes it, so an error would otherwise lose the core layer.
        if let Err(e) = RegistryBuilder::new().load_domain_pack(&name, &pack_path) {
            tracing::error!(
                error = %e,
                pack = %name,
                "lexicon pack failed to load; skipping it"
            );
            continue;
        }
        builder = builder.load_domain_pack(&name, &pack_path)?;
    }

    let user_path = root.join(USER_LEXICON_FILE);
    if user_path.exists() {
        builder = builder.load_user(load_user_lexemes(&user_path)?);
    }

    if !disabled.is_empty() {
        builder = builder.disable(disabled.to_vec());
    }
    builder.build()
}

/// Build the layered registry under `root`, honouring `Disabled` lexemes.
///
/// `LexemeStatus::Disabled` is documented as "excluded from matchers entirely"
/// and is the only user-facing way to turn a shipped word off, but nothing
/// acted on it: `LexiconMatcher::from_lexemes` never inspected `status`, and
/// `.disable(..)` was only ever called from tests (audit H17). We therefore
/// resolve the disabled ids from a first pass and rebuild with them removed —
/// the matcher then simply never sees them.
fn build_registry_at(root: &Path) -> Result<LexiconRegistry, LexiconError> {
    let first = build_layers(root, &[])?;
    let disabled = first.disabled_ids();
    if disabled.is_empty() {
        return Ok(first);
    }
    build_layers(root, &disabled)
}

/// Build the layered registry from the configured resource root.
fn build_registry() -> Result<LexiconRegistry, LexiconError> {
    build_registry_at(&crate::config::resolve_resource_path(""))
}

static REGISTRY: LazyLock<RwLock<LexiconRegistry>> = LazyLock::new(|| {
    // Resolve the core lexicon at runtime from the resource root
    // (MNEMOSYNE_HOME override, else the install root) instead of baking a
    // compile-time `env!("CARGO_MANIFEST_DIR")` into the binary — a baked
    // path made every release fail with FileLoad except on the CI builder.
    //
    // The full three-layer build (core + packs + user + disable) now actually
    // runs in production, not only in tests (audit H17).
    //
    // Two different situations, two different severities:
    // - ABSENT core: a legitimate minimal deployment; warn, do not fail.
    // - PRESENT BUT UNPARSABLE core: the operator broke the file. We still
    //   degrade to an EMPTY registry (lookups just miss) so a request never
    //   panics, but the cause is logged at ERROR level AND stashed so startup
    //   code turns it into a hard failure via [`try_init`] (audit H18).
    let core_path = crate::config::resolve_resource_path("config/dictionary.json");
    if !core_path.exists() {
        tracing::warn!(
            path = %core_path.display(),
            "core lexicon is absent; using an empty registry"
        );
        return RwLock::new(empty_registry());
    }
    let registry = match build_registry() {
        Ok(reg) => reg,
        Err(e) => {
            let msg = format!("core lexicon failed to load: {e}");
            tracing::error!(
                error = %e,
                path = %core_path.display(),
                "core lexicon failed to load; using an empty registry"
            );
            record_load_error(msg);
            empty_registry()
        }
    };
    RwLock::new(registry)
});

/// The load failure recorded by the [`REGISTRY`] initializer, if any.
static LOAD_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// Stash the singleton's load failure so [`try_init`] can surface it.
fn record_load_error(message: String) {
    // Written once during `REGISTRY` initialization, read afterwards; no user
    // code runs while the guard is held, so it cannot be poisoned.
    if let Ok(mut slot) = LOAD_ERROR.lock() {
        *slot = Some(message);
    }
}

/// Force the global registry to load and report whether it succeeded.
///
/// Startup code calls this so a broken lexicon becomes a visible startup
/// failure instead of a silent one. Returns `Err(reason)` only when the core
/// lexicon EXISTS but could not be parsed; an absent core is a legitimate
/// minimal deployment and returns `Ok(())`.
pub fn try_init() -> Result<(), String> {
    // Touch the LazyLock so the load happens now, not on the first request.
    if REGISTRY.read().is_err() {
        return Err("global lexicon registry read lock is poisoned".to_string());
    }
    match LOAD_ERROR.lock() {
        Ok(slot) => match slot.as_ref() {
            Some(reason) => Err(reason.clone()),
            None => Ok(()),
        },
        // A poisoned slot means another thread panicked mid-write; the
        // registry itself built (see above), so do not fail the process.
        Err(_) => Ok(()),
    }
}

/// Access the global registry.
pub fn global() -> std::sync::RwLockReadGuard<'static, LexiconRegistry> {
    // Read guards are released before user code can panic on them; the
    // registry is written once at startup. Expect keeps the invariant
    // traceable if poisoning ever occurs.
    REGISTRY
        .read()
        .expect("global lexicon registry read lock is not poisoned")
}

/// Reload the global registry from the default resource root.
///
/// Mirrors the singleton's layered build (core + packs + user + disable), so a
/// reload never silently drops the overlay layers.
pub fn reload() -> Result<(), LexiconError> {
    let registry = build_registry()?;
    // The assignment below cannot panic while holding the write guard, so
    // this expect never fires.
    *REGISTRY
        .write()
        .expect("global lexicon registry write lock is not poisoned") = registry;
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
