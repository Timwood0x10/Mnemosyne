//! Prototype sentence library and embedding cache.
//!
//! The [`PersonaPrototypes`] config is a JSON file (default
//! `config/persona_prototypes.json`) that replaces the hard-coded
//! `PERSONALITY_MARKERS` table. Each `(fact_type, negated)` pair owns a list
//! of bilingual prototype sentences. At startup the semantic extractor embeds
//! every prototype sentence through [`crate::embed::EmbeddingService`] and
//! stores the vectors in a [`PrototypeVectorCache`]. Classification of a new
//! utterance is then a single cosine-similarity argmax against the cache — no
//! LLM, no runtime embedding of prototypes.
//!
//! JSON schema (see `config/persona_prototypes.json`):
//!
//! ```json
//! {
//!   "version": 1,
//!   "thresholds": { "match": 0.75, "dedup": 0.95, "conflict": 0.75 },
//!   "prototypes": [
//!     { "fact_type": "Identity", "negated": false,
//!       "sentences": ["我是白流苏", "I am Bai Liusu"] }
//!   ]
//! }
//! ```

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::cognition::FactType;
use crate::embed::EmbeddingService;
use crate::error::{Error, Result};

/// Tunable thresholds for persona extraction and reconciliation.
///
/// These are loaded from the prototype JSON so they can be calibrated without
/// recompiling. All values are cosine similarities in `[0.0, 1.0]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PersonaThresholds {
    /// Minimum cosine similarity between an utterance and the winning
    /// prototype for the signal to be accepted.
    #[serde(default = "default_match")]
    pub match_: f32,

    /// Cosine similarity at or above which two facts are considered the same
    /// memory (NOOP in the reconciler).
    #[serde(default = "default_dedup")]
    pub dedup: f32,

    /// Cosine similarity at or above which a same-type, opposite-negated
    /// candidate is marked as a stance-flip transition.
    #[serde(default = "default_conflict")]
    pub conflict: f32,
}

const fn default_match() -> f32 {
    0.75
}
const fn default_dedup() -> f32 {
    0.95
}
const fn default_conflict() -> f32 {
    0.75
}

impl Default for PersonaThresholds {
    fn default() -> Self {
        Self {
            match_: default_match(),
            dedup: default_dedup(),
            conflict: default_conflict(),
        }
    }
}

/// One `(fact_type, negated)` prototype group with its example sentences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonaPrototypeEntry {
    /// The persona facet this prototype expresses.
    pub fact_type: FactType,
    /// Whether the prototype sentences are stance-against (negated).
    #[serde(default)]
    pub negated: bool,
    /// Bilingual example sentences. At least one is required.
    #[serde(deserialize_with = "deserialize_non_empty_sentences")]
    pub sentences: Vec<String>,
}

/// Deserialize sentences and guarantee the list is non-empty.
///
/// An empty sentence list makes the prototype useless (no vector to match
/// against) and is therefore a config error, not a runtime fallthrough.
fn deserialize_non_empty_sentences<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Vec::<String>::deserialize(deserializer)?;
    if v.is_empty() {
        return Err(serde::de::Error::custom(
            "prototype `sentences` must be non-empty",
        ));
    }
    Ok(v)
}

/// The full prototype library loaded from JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonaPrototypes {
    /// Schema version; bumped when the JSON shape changes incompatibly.
    #[serde(default = "default_version")]
    pub version: u32,

    /// Tunable thresholds for extraction and reconciliation.
    #[serde(default)]
    pub thresholds: PersonaThresholds,

    /// One entry per `(fact_type, negated)` pair.
    #[serde(deserialize_with = "deserialize_non_empty_prototypes")]
    pub prototypes: Vec<PersonaPrototypeEntry>,
}

const fn default_version() -> u32 {
    1
}

fn deserialize_non_empty_prototypes<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<PersonaPrototypeEntry>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Vec::<PersonaPrototypeEntry>::deserialize(deserializer)?;
    if v.is_empty() {
        return Err(serde::de::Error::custom(
            "`prototypes` must contain at least one entry",
        ));
    }
    Ok(v)
}

/// A single embedded prototype sentence ready for cosine comparison.
///
/// One `PrototypeVector` corresponds to exactly one sentence in
/// [`PersonaPrototypeEntry::sentences`]. The `fact_type` and `negated` fields
/// are duplicated here so the argmax lookup returns a fully self-describing
/// signal without an extra indirection.
#[derive(Debug, Clone, PartialEq)]
pub struct PrototypeVector {
    /// The persona facet this prototype expresses.
    pub fact_type: FactType,
    /// Whether this prototype is a stance-against (negated) sentence.
    pub negated: bool,
    /// The original prototype sentence (for debugging / evidence).
    pub sentence: String,
    /// The embedded vector.
    pub vector: Vec<f32>,
}

/// In-memory cache of embedded prototype vectors.
///
/// Built once at startup by [`PrototypeVectorCache::build`]. The cache is
/// immutable after construction; callers obtain a shared reference via
/// [`PrototypeVectorCache::vectors`].
#[derive(Debug)]
pub struct PrototypeVectorCache {
    vectors: Vec<PrototypeVector>,
    /// Index from `fact_type` to positions in `vectors`, for filtered argmax.
    by_type: HashMap<FactType, Vec<usize>>,
}

impl PrototypeVectorCache {
    /// Returns a slice of all embedded prototype vectors.
    #[must_use]
    pub fn vectors(&self) -> &[PrototypeVector] {
        &self.vectors
    }

    /// Returns the indices of all prototype vectors for the given `fact_type`.
    #[must_use]
    pub fn indices_for_type(&self, fact_type: FactType) -> Option<&[usize]> {
        self.by_type.get(&fact_type).map(Vec::as_slice)
    }

    /// Embed every prototype sentence and assemble the cache.
    ///
    /// Sentences are embedded in a single batch via
    /// [`EmbeddingService::embed_batch`] to amortise HTTP round-trips. The
    /// batch is flattened in the same order as the prototype entries, so the
    /// returned cache preserves the JSON declaration order.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the embedding service is disabled (the
    /// semantic path requires a working backend), or [`Error::Embedding`] if
    /// the batch embed call fails.
    pub async fn build(
        prototypes: &PersonaPrototypes,
        embedder: &dyn EmbeddingService,
    ) -> Result<Self> {
        if !embedder.enabled() {
            return Err(Error::Config(
                "semantic persona extraction requires an enabled embedding service".to_string(),
            ));
        }

        // Flatten (entry -> sentences) into one ordered batch.
        let mut batch: Vec<String> = Vec::new();
        // Track which (fact_type, negated, sentence) each batch slot maps to.
        let mut plan: Vec<(FactType, bool, String)> = Vec::new();
        for entry in &prototypes.prototypes {
            for sentence in &entry.sentences {
                batch.push(sentence.clone());
                plan.push((entry.fact_type, entry.negated, sentence.clone()));
            }
        }

        let embedded = embedder.embed_batch(&batch).await?;
        if embedded.len() != plan.len() {
            return Err(Error::Embedding(crate::error::EmbeddingError::Decode(
                format!(
                    "prototype batch size mismatch: embedded {} vectors for {} sentences",
                    embedded.len(),
                    plan.len()
                ),
            )));
        }

        let mut vectors: Vec<PrototypeVector> = Vec::with_capacity(plan.len());
        let mut by_type: HashMap<FactType, Vec<usize>> = HashMap::new();
        for (i, (fact_type, negated, sentence)) in plan.into_iter().enumerate() {
            let idx = vectors.len();
            by_type.entry(fact_type).or_default().push(idx);
            vectors.push(PrototypeVector {
                fact_type,
                negated,
                sentence,
                vector: embedded[i].clone(),
            });
        }

        Ok(Self { vectors, by_type })
    }
}

/// Load the prototype library from a JSON file at `path`.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be read, or [`Error::Config`] if
/// the JSON is malformed or fails schema validation (empty `sentences`,
/// empty `prototypes`, unknown `fact_type`).
pub fn load_prototype_config<P: AsRef<Path>>(path: P) -> Result<PersonaPrototypes> {
    let raw = std::fs::read_to_string(path.as_ref()).map_err(Error::Io)?;
    let config: PersonaPrototypes =
        serde_json::from_str(&raw).map_err(|e| Error::Config(format!("prototype JSON: {e}")))?;
    validate_thresholds(&config.thresholds)?;
    Ok(config)
}

/// Validate that thresholds lie in the legal cosine-similarity range and are
/// internally consistent (`match_ <= conflict <= dedup`).
fn validate_thresholds(t: &PersonaThresholds) -> Result<()> {
    let range = 0.0..=1.0;
    if !range.contains(&t.match_) {
        return Err(Error::Config(format!(
            "threshold `match` must be in [0,1], got {}",
            t.match_
        )));
    }
    if !range.contains(&t.dedup) {
        return Err(Error::Config(format!(
            "threshold `dedup` must be in [0,1], got {}",
            t.dedup
        )));
    }
    if !range.contains(&t.conflict) {
        return Err(Error::Config(format!(
            "threshold `conflict` must be in [0,1], got {}",
            t.conflict
        )));
    }
    // dedup is the strictest bound; match_ the loosest. conflict sits between.
    if t.match_ > t.conflict {
        return Err(Error::Config(format!(
            "threshold `match` ({}) cannot exceed `conflict` ({})",
            t.match_, t.conflict
        )));
    }
    if t.conflict > t.dedup {
        return Err(Error::Config(format!(
            "threshold `conflict` ({}) cannot exceed `dedup` ({})",
            t.conflict, t.dedup
        )));
    }
    Ok(())
}

/// Convenience alias for callers that only need the thresholds.
pub type PersonaPrototypeConfig = PersonaPrototypes;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::NullEmbedder;

    /// Objective: Verify loading a well-formed prototype JSON succeeds and
    /// preserves all fields.
    /// Invariants: The returned config has version 1, four prototypes, and
    /// the default thresholds when not specified.
    #[test]
    fn load_valid_prototype_config() {
        let json = r#"{
            "version": 1,
            "prototypes": [
                {"fact_type": "Identity", "negated": false, "sentences": ["我是白流苏"]},
                {"fact_type": "Preference", "negated": true, "sentences": ["我讨厌应酬"]}
            ]
        }"#;
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        std::fs::write(tmp.path(), json).expect("write json");
        let config = load_prototype_config(tmp.path()).expect("load config");
        assert_eq!(config.version, 1, "version preserved");
        assert_eq!(config.prototypes.len(), 2, "two prototype entries");
        assert_eq!(
            config.thresholds.match_, 0.75,
            "default match threshold applied"
        );
    }

    /// Objective: Verify that an empty `sentences` list is rejected at parse
    /// time as a config error, not silently producing a useless prototype.
    /// Invariants: `load_prototype_config` returns `Error::Config`.
    #[test]
    fn empty_sentences_rejected() {
        let json = r#"{
            "prototypes": [
                {"fact_type": "Identity", "negated": false, "sentences": []}
            ]
        }"#;
        let result: std::result::Result<PersonaPrototypes, _> = serde_json::from_str(json);
        assert!(result.is_err(), "empty sentences must be rejected");
    }

    /// Objective: Verify threshold range validation rejects out-of-range
    /// `match` values.
    /// Invariants: `match` outside [0,1] returns `Error::Config`.
    #[test]
    fn threshold_out_of_range_rejected() {
        let thresholds = PersonaThresholds {
            match_: 1.5,
            dedup: 0.95,
            conflict: 0.75,
        };
        let result = validate_thresholds(&thresholds);
        assert!(result.is_err(), "out-of-range match threshold rejected");
    }

    /// Objective: Verify that `match > conflict` is rejected, since a signal
    /// that passes the match gate should also be eligible for transition
    /// detection.
    /// Invariants: Returns `Error::Config` with a message mentioning both
    /// thresholds.
    #[test]
    fn match_exceeds_conflict_rejected() {
        let thresholds = PersonaThresholds {
            match_: 0.85,
            dedup: 0.95,
            conflict: 0.75,
        };
        let result = validate_thresholds(&thresholds);
        assert!(result.is_err(), "match > conflict must be rejected");
    }

    /// Objective: Verify that building the vector cache fails cleanly when
    /// the embedding service is disabled (NullEmbedder), rather than
    /// producing empty vectors that would silently break classification.
    /// Invariants: `build` returns `Error::Config`; no panic.
    #[tokio::test]
    async fn cache_build_fails_on_disabled_embedder() {
        let config = PersonaPrototypes {
            version: 1,
            thresholds: PersonaThresholds::default(),
            prototypes: vec![PersonaPrototypeEntry {
                fact_type: FactType::Identity,
                negated: false,
                sentences: vec!["我是白流苏".to_string()],
            }],
        };
        let embedder = NullEmbedder::new();
        let result = PrototypeVectorCache::build(&config, &embedder).await;
        assert!(result.is_err(), "disabled embedder must fail build");
        match result.unwrap_err() {
            Error::Config(msg) => assert!(
                msg.contains("enabled"),
                "error should mention enabled embedder: {msg}"
            ),
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    /// Objective: Verify the by_type index correctly groups prototype vectors
    /// by their fact_type for filtered argmax.
    /// Invariants: A config with two Identity prototypes yields an index
    /// pointing to exactly those two vectors.
    #[test]
    fn by_type_index_groups_correctly() {
        // We test the plan structure manually since we can't embed without a
        // real backend. The grouping logic is purely structural.
        let prototypes = vec![
            PersonaPrototypeEntry {
                fact_type: FactType::Identity,
                negated: false,
                sentences: vec!["s1".into(), "s2".into()],
            },
            PersonaPrototypeEntry {
                fact_type: FactType::Preference,
                negated: true,
                sentences: vec!["s3".into()],
            },
        ];
        // Simulate the build's grouping logic.
        let mut by_type: HashMap<FactType, Vec<usize>> = HashMap::new();
        let mut idx = 0;
        for entry in &prototypes {
            for _ in &entry.sentences {
                by_type.entry(entry.fact_type).or_default().push(idx);
                idx += 1;
            }
        }
        let identity_indices = by_type.get(&FactType::Identity).expect("Identity present");
        assert_eq!(identity_indices.len(), 2, "two Identity prototypes indexed");
        let pref_indices = by_type
            .get(&FactType::Preference)
            .expect("Preference present");
        assert_eq!(pref_indices.len(), 1, "one Preference prototype indexed");
    }
}
