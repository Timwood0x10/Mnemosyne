use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Parser;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Resolve a runtime resource file (config/lexicon JSON) to a usable path.
///
/// A single optional override — `MNEMOSYNE_HOME` — sets the resource root;
/// when unset the root is auto-detected: the current working directory if it
/// looks like an install root (contains `config/` or `lexicon/`), otherwise
/// the executable's directory. `relative` is a path under that root
/// (`config/dictionary.json`, `lexicon/packs/…`).
///
/// This replaces the previous per-file `*_PATH` env vars (DICTIONARY_PATH,
/// FACTION_MAP_PATH, …) with one root override, and kills the compile-time
/// `env!("CARGO_MANIFEST_DIR")` baked into the binary (which made releases
/// fail on every machine except the CI builder).
#[must_use]
pub fn resolve_resource_path(relative: &str) -> PathBuf {
    resource_root().join(relative)
}

/// Locate the resource root: `MNEMOSYNE_HOME` if set, else an install-looking
/// cwd, else the executable's directory, else the cwd as a last resort.
fn resource_root() -> PathBuf {
    if let Ok(home) = std::env::var("MNEMOSYNE_HOME") {
        if !home.is_empty() {
            return PathBuf::from(home);
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if looks_like_root(&cwd) {
        return cwd;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent()
            && looks_like_root(dir)
        {
            return dir.to_path_buf();
        }
    }
    cwd
}

/// An install root carries either `config/` or `lexicon/` (the two resource
/// trees the server reads at runtime).
fn looks_like_root(dir: &Path) -> bool {
    dir.join("config").is_dir() || dir.join("lexicon").is_dir()
}

/// Top-level server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// SQLite database file path. Use `:memory:` for ephemeral storage.
    pub db_path: String,

    /// Vector embedding dimension. Must match the upstream model.
    pub vector_dim: usize,

    /// Upstream embedding service base URL (no trailing slash).
    pub embedding_url: String,

    /// Embedding model identifier sent in the `X-Model` request header.
    pub embedding_model: String,

    /// Per-request embedding timeout.
    pub embedding_timeout: Duration,

    /// Minimum importance to keep a distilled memory, in `[0.0, 1.0]`.
    pub min_importance: f64,

    /// Cosine similarity above which two memories are considered conflicting.
    pub conflict_threshold: f64,

    /// Maximum memories produced per distillation call.
    pub max_memories_per_distillation: usize,

    /// Maximum `Knowledge` memories retained per tenant.
    pub max_solutions_per_tenant: usize,

    /// Enable cross-turn experience extraction.
    pub enable_cross_turn: bool,

    /// Embedding provider selection.
    ///
    /// - `none` (default): no embeddings; retrieval falls back to keyword
    ///   search only. The server runs fully without any embedding backend.
    /// - `openai`: use OpenAI embeddings (requires `MEMORY_OPENAI_API_KEY`).
    /// - `ollama`: use a local Ollama embedding model.
    pub embedding_provider: EmbeddingProvider,

    /// Retrieval mode selection.
    ///
    /// - `keyword` (default when no embedding): BM25/FTS keyword search.
    /// - `vector`: vector similarity search (requires embedding).
    /// - `hybrid`: keyword + vector + ranking fusion (requires embedding).
    pub retrieval_mode: RetrievalMode,

    /// Optional OpenAI API key. Required when `embedding_provider == openai`.
    pub openai_api_key: Option<String>,
}

/// Available embedding providers, pluggable per `improve.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingProvider {
    /// No embeddings; retrieval uses keyword/metadata search only.
    #[default]
    None,
    /// OpenAI embeddings API.
    Openai,
    /// Local Ollama embedding model.
    Ollama,
}

impl EmbeddingProvider {
    /// String identifier used in env vars and CLI args.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            EmbeddingProvider::None => "none",
            EmbeddingProvider::Openai => "openai",
            EmbeddingProvider::Ollama => "ollama",
        }
    }

    /// Returns `true` when this provider actually produces embeddings.
    ///
    /// `None` is the disabled case per `improve.md` Principle 1.
    #[must_use]
    pub fn produces_embeddings(self) -> bool {
        !matches!(self, EmbeddingProvider::None)
    }
}

impl std::fmt::Display for EmbeddingProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for EmbeddingProvider {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "none" | "null" | "disabled" => Ok(EmbeddingProvider::None),
            "openai" => Ok(EmbeddingProvider::Openai),
            "ollama" => Ok(EmbeddingProvider::Ollama),
            other => Err(format!("unknown embedding provider: {other}")),
        }
    }
}

/// Available retrieval modes per `improve.md` Section 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RetrievalMode {
    /// Keyword-only search (default when no embedding provider is configured).
    #[default]
    Keyword,
    /// Vector similarity search (requires an embedding provider).
    Vector,
    /// Hybrid retrieval: keyword + vector + ranking fusion.
    Hybrid,
}

/// Ranking-fusion weights per `improve.md` Section 7.
///
/// When embeddings are present:
/// `score = semantic * WEIGHT_SEMANTIC + keyword * WEIGHT_KEYWORD_HYBRID + importance * WEIGHT_IMPORTANCE_HYBRID`.
///
/// When no embedding is available (keyword-only mode):
/// `score = keyword * WEIGHT_KEYWORD_ONLY + importance * WEIGHT_IMPORTANCE_ONLY`.
pub const WEIGHT_SEMANTIC: f64 = 0.6;
pub const WEIGHT_KEYWORD_HYBRID: f64 = 0.2;
pub const WEIGHT_IMPORTANCE_HYBRID: f64 = 0.2;
/// Temporal-relevance weight in hybrid fusion (mem0-style time-aware
/// retrieval). Scaled by `temporal_score` in [0,1]; kept small so time is a
/// decisive-but-bounded tiebreaker, never a dominant signal.
pub const WEIGHT_TEMPORAL_HYBRID: f64 = 0.15;
/// Exponential temporal-decay rate λ (per second) for
/// `score = e^(−λ·Δt)`. Default ≈ 0.01/day ≈ 1.157e-7/s, so a 90-day-old
/// memory scores e^−0.9 ≈ 0.41. Repeated occurrence refreshes the memory
/// timestamp, resetting the decay (long-term preferences stay fresh).
pub const TEMPORAL_DECAY_LAMBDA_PER_SEC: f64 = 1.157e-7;
/// Default context-usage threshold (percent) that triggers distillation +
/// memory injection in `memory_context_check`. Configurable per call via the
/// `threshold` argument; this is the fallback when none is supplied.
pub const CONTEXT_INJECT_THRESHOLD: f64 = 40.0;
/// Path (relative to the working directory) to the persona prototype library
/// JSON used by the semantic persona-extraction path. Overridable at runtime
/// via the `PERSONA_PROTOTYPES_PATH` environment variable.
pub const PERSONA_PROTOTYPES_PATH: &str = "config/persona_prototypes.json";
/// Path (relative to the working directory) to the structured persona card
/// JSON used by the `persona_inject` MCP tool. Overridable at runtime via the
/// `PERSONA_CARDS_PATH` environment variable. The file is optional: when it
/// is absent the tool falls back to the aggregated persona card only.
pub const PERSONA_CARDS_PATH: &str = "config/persona_cards.json";
/// Grayscale gate for the pure-embedding memory features (plan
/// `embedding-memory-plan.md`, P0–P3).
///
/// **Default OFF**: the embedding-memory modules (`anchor`, `centroid`) exist
/// as independent additions, but the main-line behaviors they would change
/// (exponential temporal decay in retrieval, `inject_memories` in
/// `memory_context_check`) stay at their legacy values until this flag is
/// flipped. Set to `true` only after the grayscale modules pass full tests.
pub const EMBEDDING_MEMORY_GRAYSCALE: bool = false;
pub const WEIGHT_KEYWORD_ONLY: f64 = 0.7;
pub const WEIGHT_IMPORTANCE_ONLY: f64 = 0.3;

impl RetrievalMode {
    /// String identifier used in env vars and CLI args.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RetrievalMode::Keyword => "keyword",
            RetrievalMode::Vector => "vector",
            RetrievalMode::Hybrid => "hybrid",
        }
    }
}

impl std::fmt::Display for RetrievalMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for RetrievalMode {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "keyword" | "bm25" | "fts" => Ok(RetrievalMode::Keyword),
            "vector" => Ok(RetrievalMode::Vector),
            "hybrid" => Ok(RetrievalMode::Hybrid),
            other => Err(format!("unknown retrieval mode: {other}")),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            db_path: "./memory.db".to_string(),
            vector_dim: 0,
            embedding_url: "http://localhost:8000".to_string(),
            embedding_model: "e5-large".to_string(),
            embedding_timeout: Duration::from_secs(30),
            min_importance: 0.6,
            conflict_threshold: 0.85,
            max_memories_per_distillation: 3,
            max_solutions_per_tenant: 5000,
            enable_cross_turn: true,
            embedding_provider: EmbeddingProvider::None,
            retrieval_mode: RetrievalMode::Keyword,
            openai_api_key: None,
        }
    }
}

impl Config {
    /// Load configuration from environment variables, applying defaults
    /// for any variable that is unset. A variable that IS set but fails to
    /// parse is an error — silently substituting the default discarded the
    /// operator's explicit configuration without warning.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when a set variable cannot be parsed or when
    /// [`Self::validate`] rejects the resulting values.
    pub fn from_env() -> Result<Self> {
        fn parse_env<T>(name: &str) -> crate::error::Result<Option<T>>
        where
            T: std::str::FromStr,
            T::Err: std::fmt::Display,
        {
            match std::env::var(name) {
                Err(_) => Ok(None),
                Ok(v) => v
                    .parse::<T>()
                    .map(Some)
                    .map_err(|e| Error::Config(format!("invalid {name}={v:?}: {e}"))),
            }
        }

        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("MEMORY_DB_PATH") {
            cfg.db_path = v;
        }
        if let Some(dim) = parse_env::<usize>("MEMORY_VECTOR_DIM")? {
            cfg.vector_dim = dim;
        }
        if let Ok(v) = std::env::var("MEMORY_EMBEDDING_URL") {
            cfg.embedding_url = v;
        }
        if let Ok(v) = std::env::var("MEMORY_EMBEDDING_MODEL") {
            cfg.embedding_model = v;
        }
        if let Some(ms) = parse_env::<u64>("MEMORY_EMBEDDING_TIMEOUT_MS")? {
            cfg.embedding_timeout = Duration::from_millis(ms);
        }
        if let Some(f) = parse_env::<f64>("MEMORY_MIN_IMPORTANCE")? {
            cfg.min_importance = f;
        }
        if let Some(f) = parse_env::<f64>("MEMORY_CONFLICT_THRESHOLD")? {
            cfg.conflict_threshold = f;
        }
        if let Some(n) = parse_env::<usize>("MEMORY_MAX_PER_DISTILL")? {
            cfg.max_memories_per_distillation = n;
        }
        if let Some(n) = parse_env::<usize>("MEMORY_MAX_SOLUTIONS")? {
            cfg.max_solutions_per_tenant = n;
        }
        if let Ok(v) = std::env::var("MEMORY_DISABLE_CROSS_TURN")
            && (v == "1" || v.eq_ignore_ascii_case("true"))
        {
            cfg.enable_cross_turn = false;
        }
        if let Some(p) = parse_env::<EmbeddingProvider>("MEMORY_EMBEDDING_PROVIDER")? {
            cfg.embedding_provider = p;
        }
        if let Some(m) = parse_env::<RetrievalMode>("MEMORY_RETRIEVAL_MODE")? {
            cfg.retrieval_mode = m;
        }
        if let Ok(v) = std::env::var("MEMORY_OPENAI_API_KEY") {
            cfg.openai_api_key = Some(v);
        }
        cfg.validate()?;
        Ok(cfg)
    }

    /// Validate the configuration values.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when:
    /// - `vector_dim` is 0 when `embedding_provider` produces embeddings.
    /// - `min_importance` or `conflict_threshold` are outside `[0.0, 1.0]`.
    /// - `max_memories_per_distillation` or `max_solutions_per_tenant` are 0.
    /// - `embedding_provider` is set but no OpenAI API key is present.
    /// - `retrieval_mode` requires embeddings but `embedding_provider` is `None`.
    pub fn validate(&self) -> Result<()> {
        if self.embedding_provider.produces_embeddings() && self.vector_dim == 0 {
            return Err(Error::Config(
                "vector_dim must be > 0 when embedding provider is enabled".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.min_importance) {
            return Err(Error::Config(format!(
                "min_importance must be in [0, 1], got {}",
                self.min_importance
            )));
        }
        if !(0.0..=1.0).contains(&self.conflict_threshold) {
            return Err(Error::Config(format!(
                "conflict_threshold must be in [0, 1], got {}",
                self.conflict_threshold
            )));
        }
        if self.max_memories_per_distillation == 0 {
            return Err(Error::Config(
                "max_memories_per_distillation must be > 0".into(),
            ));
        }
        if self.max_solutions_per_tenant == 0 {
            return Err(Error::Config("max_solutions_per_tenant must be > 0".into()));
        }
        if self.embedding_provider == EmbeddingProvider::Openai && self.openai_api_key.is_none() {
            return Err(Error::Config(
                "embedding_provider=openai requires MEMORY_OPENAI_API_KEY".into(),
            ));
        }
        let needs_embedding = matches!(
            self.retrieval_mode,
            RetrievalMode::Vector | RetrievalMode::Hybrid
        );
        if needs_embedding && !self.embedding_provider.produces_embeddings() {
            return Err(Error::Config(format!(
                "retrieval_mode={retrieval} requires an embedding provider, got {provider}",
                retrieval = self.retrieval_mode,
                provider = self.embedding_provider
            )));
        }
        Ok(())
    }
}

/// Command-line arguments parsed by clap.
#[derive(Debug, Clone, Parser)]
#[command(name = "memory-mcp", version, about = "Memory Distillation MCP Server")]
pub struct CliArgs {
    /// Subcommand (only `serve` is supported).
    #[command(subcommand)]
    pub command: Option<Command>,

    /// SQLite database file path.
    #[arg(long, env = "MEMORY_DB_PATH", default_value = "./memory.db")]
    pub db_path: String,

    /// Vector embedding dimension. Set to 0 for keyword-only mode (FTS5).
    #[arg(long, env = "MEMORY_VECTOR_DIM", default_value_t = 0)]
    pub vector_dim: usize,

    /// Upstream embedding service URL.
    #[arg(
        long,
        env = "MEMORY_EMBEDDING_URL",
        default_value = "http://localhost:8000"
    )]
    pub embedding_url: String,

    /// Embedding model identifier.
    #[arg(long, env = "MEMORY_EMBEDDING_MODEL", default_value = "e5-large")]
    pub embedding_model: String,

    /// Embedding request timeout in milliseconds.
    #[arg(long, env = "MEMORY_EMBEDDING_TIMEOUT_MS", default_value_t = 30_000)]
    pub embedding_timeout_ms: u64,

    /// Minimum importance score to keep a distilled memory.
    #[arg(long, env = "MEMORY_MIN_IMPORTANCE", default_value_t = 0.6)]
    pub min_importance: f64,

    /// Cosine similarity threshold for conflict detection.
    #[arg(long, env = "MEMORY_CONFLICT_THRESHOLD", default_value_t = 0.85)]
    pub conflict_threshold: f64,

    /// Maximum memories produced per distillation call.
    #[arg(long, env = "MEMORY_MAX_PER_DISTILL", default_value_t = 3)]
    pub max_memories_per_distillation: usize,

    /// Maximum `Knowledge` memories retained per tenant.
    #[arg(long, env = "MEMORY_MAX_SOLUTIONS", default_value_t = 5000)]
    pub max_solutions_per_tenant: usize,

    /// Disable cross-turn experience extraction.
    #[arg(long, env = "MEMORY_DISABLE_CROSS_TURN", default_value_t = false)]
    pub disable_cross_turn: bool,

    /// Embedding provider: `none` (default), `openai`, or `ollama`.
    #[arg(long, env = "MEMORY_EMBEDDING_PROVIDER", default_value = "none")]
    pub embedding_provider: String,

    /// Retrieval mode: `keyword` (default), `vector`, or `hybrid`.
    #[arg(long, env = "MEMORY_RETRIEVAL_MODE", default_value = "keyword")]
    pub retrieval_mode: String,

    /// Optional OpenAI API key (required when embedding-provider=openai).
    #[arg(long, env = "MEMORY_OPENAI_API_KEY")]
    pub openai_api_key: Option<String>,

    /// Serve transport: `stdio` (default, line-delimited JSON on stdio) or
    /// `http` (MCP Streamable HTTP over SSE on `--addr`).
    #[arg(long, env = "MEMORY_TRANSPORT", default_value = "stdio")]
    pub transport: String,

    /// Listen address for `--transport http` (e.g. `127.0.0.1:5609`).
    #[arg(long, env = "MEMORY_HTTP_ADDR", default_value = "127.0.0.1:5609")]
    pub http_addr: String,

    /// Optional bearer token required for `--transport http`. When unset the
    /// HTTP endpoints are served without authentication (local trust only).
    #[arg(long, env = "MEMORY_HTTP_TOKEN")]
    pub http_token: Option<String>,
}

/// Supported subcommands.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum Command {
    /// Run the MCP server (default behavior if no subcommand given).
    Serve,
    /// Distill character knowledge graph from corpus text files.
    Ingest {
        /// Path to corpus text files directory.
        #[arg(long, default_value = "corpus")]
        corpus_dir: String,
    },
    /// Migrate V1 domain data (`character_*` tables + corpus text) into the
    /// general knowledge model (`knowledge_objects` / `knowledge_edges` /
    /// `evidence` / ...). One-time operation per dev_guide §6.
    Migrate {
        /// Path to corpus text files directory (used for documents, chapters,
        /// and original-text evidence).
        #[arg(long, default_value = "corpus")]
        corpus_dir: String,
    },
    /// Report how the runtime resource files loaded — which marker files were
    /// read, what each contributed, the effective word count per action, and
    /// every entry that was dropped — without starting the server. Exits
    /// non-zero when the marker table cannot be trusted.
    ConfigCheck,
}

impl CliArgs {
    /// Convert parsed CLI args into a [`Config`], applying env-var fallbacks
    /// for any field not explicitly set on the command line.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the resulting config fails validation.
    pub fn into_config(self) -> Result<Config> {
        let embedding_provider = self
            .embedding_provider
            .parse::<EmbeddingProvider>()
            .map_err(Error::Config)?;
        let retrieval_mode = self
            .retrieval_mode
            .parse::<RetrievalMode>()
            .map_err(Error::Config)?;
        let mut cfg = Config {
            db_path: self.db_path,
            vector_dim: self.vector_dim,
            embedding_url: self.embedding_url,
            embedding_model: self.embedding_model,
            embedding_timeout: Duration::from_millis(self.embedding_timeout_ms),
            min_importance: self.min_importance,
            conflict_threshold: self.conflict_threshold,
            max_memories_per_distillation: self.max_memories_per_distillation,
            max_solutions_per_tenant: self.max_solutions_per_tenant,
            enable_cross_turn: !self.disable_cross_turn,
            embedding_provider,
            retrieval_mode,
            openai_api_key: self.openai_api_key,
        };
        // Auto-set vector_dim=0 when no embeddings are used
        if !cfg.embedding_provider.produces_embeddings() {
            cfg.vector_dim = 0;
        }
        cfg.validate()?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_validates() {
        assert!(
            Config::default().validate().is_ok(),
            "default should validate"
        );
    }

    #[test]
    fn validate_rejects_zero_dim() {
        let cfg = Config {
            vector_dim: 0,
            embedding_provider: EmbeddingProvider::Openai,
            openai_api_key: Some("sk-test".into()),
            ..Config::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, Error::Config(_)), "expected Config error");
    }

    #[test]
    fn validate_rejects_bad_min_importance() {
        let cfg = Config {
            min_importance: 1.7,
            ..Config::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("min_importance"),
            "error mentions min_importance"
        );
    }

    #[test]
    fn validate_rejects_bad_threshold() {
        let cfg = Config {
            conflict_threshold: -0.1,
            ..Config::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("conflict_threshold"),
            "error mentions conflict_threshold"
        );
    }

    #[test]
    fn cli_args_into_config() {
        let args = CliArgs {
            command: None,
            db_path: "/tmp/test.db".into(),
            vector_dim: 512,
            embedding_url: "http://localhost:8000".into(),
            embedding_model: "e5-large".into(),
            embedding_timeout_ms: 60_000,
            min_importance: 0.5,
            conflict_threshold: 0.9,
            max_memories_per_distillation: 5,
            max_solutions_per_tenant: 1000,
            disable_cross_turn: true,
            embedding_provider: "none".into(),
            retrieval_mode: "keyword".into(),
            openai_api_key: None,
            transport: "stdio".into(),
            http_addr: "127.0.0.1:5609".into(),
            http_token: None,
        };
        let cfg = args.into_config().expect("config");
        assert_eq!(cfg.db_path, "/tmp/test.db");
        assert_eq!(cfg.vector_dim, 0, "auto-set to 0 when provider=none");
        assert!(!cfg.enable_cross_turn, "cross-turn disabled");
        assert_eq!(cfg.embedding_timeout, Duration::from_millis(60_000));
        assert_eq!(cfg.embedding_provider, EmbeddingProvider::None);
        assert_eq!(cfg.retrieval_mode, RetrievalMode::Keyword);
    }

    #[test]
    fn validate_rejects_openai_without_key() {
        let cfg = Config {
            embedding_provider: EmbeddingProvider::Openai,
            vector_dim: 768,
            openai_api_key: None,
            ..Config::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("MEMORY_OPENAI_API_KEY"),
            "error must mention the missing API key, got: {err}"
        );
    }

    #[test]
    fn validate_rejects_vector_retrieval_without_embedding() {
        let cfg = Config {
            embedding_provider: EmbeddingProvider::None,
            retrieval_mode: RetrievalMode::Vector,
            ..Config::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("retrieval_mode=vector"),
            "error must mention the retrieval mode mismatch, got: {err}"
        );
    }

    #[test]
    fn validate_rejects_hybrid_retrieval_without_embedding() {
        let cfg = Config {
            embedding_provider: EmbeddingProvider::None,
            retrieval_mode: RetrievalMode::Hybrid,
            ..Config::default()
        };
        assert!(cfg.validate().is_err(), "hybrid+none must fail validate");
    }

    #[test]
    fn validate_accepts_hybrid_with_embedding_provider() {
        let cfg = Config {
            embedding_provider: EmbeddingProvider::Openai,
            vector_dim: 768,
            openai_api_key: Some("sk-test".into()),
            retrieval_mode: RetrievalMode::Hybrid,
            ..Config::default()
        };
        assert!(cfg.validate().is_ok(), "hybrid+openai must validate");
    }

    #[test]
    fn embedding_provider_parses_aliases() {
        for s in ["none", "null", "disabled", "NONE", "Null"] {
            let p = s.parse::<EmbeddingProvider>().expect("parse");
            assert_eq!(p, EmbeddingProvider::None, "alias {s} -> None");
        }
        assert_eq!(
            "openai".parse::<EmbeddingProvider>().unwrap(),
            EmbeddingProvider::Openai
        );
        assert_eq!(
            "ollama".parse::<EmbeddingProvider>().unwrap(),
            EmbeddingProvider::Ollama
        );
    }

    #[test]
    fn embedding_provider_rejects_unknown() {
        let err = "fastembed".parse::<EmbeddingProvider>().unwrap_err();
        assert!(err.contains("fastembed"), "error echoes bad input");
    }

    #[test]
    fn embedding_provider_produces_embeddings_flag() {
        assert!(
            !EmbeddingProvider::None.produces_embeddings(),
            "None must not produce embeddings"
        );
        assert!(
            EmbeddingProvider::Openai.produces_embeddings(),
            "Openai must produce embeddings"
        );
        assert!(
            EmbeddingProvider::Ollama.produces_embeddings(),
            "Ollama must produce embeddings"
        );
    }

    #[test]
    fn from_env_uses_defaults_when_unset() {
        let keys = [
            "MEMORY_DB_PATH",
            "MEMORY_VECTOR_DIM",
            "MEMORY_EMBEDDING_URL",
            "MEMORY_EMBEDDING_MODEL",
            "MEMORY_EMBEDDING_TIMEOUT_MS",
            "MEMORY_MIN_IMPORTANCE",
            "MEMORY_CONFLICT_THRESHOLD",
            "MEMORY_MAX_PER_DISTILL",
            "MEMORY_MAX_SOLUTIONS",
            "MEMORY_DISABLE_CROSS_TURN",
        ];
        let saved: Vec<(String, Option<String>)> = keys
            .iter()
            .map(|k| (k.to_string(), std::env::var(k).ok()))
            .collect();
        for k in &keys {
            unsafe { std::env::remove_var(k) };
        }
        let cfg = Config::from_env().expect("valid default config");
        for (k, v) in &saved {
            if let Some(val) = v {
                unsafe { std::env::set_var(k, val) };
            }
        }
        let default = Config::default();
        assert_eq!(cfg.db_path, default.db_path);
        assert_eq!(cfg.vector_dim, 0);
        assert_eq!(cfg.min_importance, default.min_importance);
        assert_eq!(cfg.enable_cross_turn, default.enable_cross_turn);
    }
}
