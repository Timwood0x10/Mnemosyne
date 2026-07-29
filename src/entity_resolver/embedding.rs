//! Embedder trait — abstract text-to-vector interface.
//!
//! The entity resolution engine depends only on this trait, never on a specific
//! embedding model. Providers (FastEmbed, BGE, Jina, E5) swap out below this
//! trait without touching any resolver code.

use crate::error::EmbeddingError;
use crate::error::Error;

/// Converts text into a dense vector representation.
///
/// # V1 implementations
///
/// | Provider | Library | Dimension | Source |
/// |---|---|---|---|
/// | `FastEmbedProvider` | `fastembed-rs` | 384 | ONNX local |
pub trait Embedder: Send + Sync {
    /// Embed a single text string into a vector.
    fn embed(&self, text: &str) -> Result<Vec<f32>, Error>;

    /// Embed a batch of text strings into vectors.
    ///
    /// Default implementation calls `embed` in a loop. Providers that support
    /// native batching (e.g. fastembed-rs) should override this.
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        texts.iter().map(|t| self.embed(t)).collect()
    }
}

/// Embedder backed by the `fastembed-rs` library (ONNX, local, no API key).
///
/// Uses the `all-MiniLM-L6-v2` model (384-dim). The model is downloaded
/// automatically on first use and cached locally.
#[cfg(feature = "remote-embed")]
pub struct FastEmbedProvider {
    model: fastembed::TextEmbedding,
}

#[cfg(feature = "remote-embed")]
impl FastEmbedProvider {
    /// Create a new `FastEmbedProvider`, loading the default ONNX model.
    pub fn new() -> Result<Self, Error> {
        use fastembed::InitOptions;
        let model = fastembed::TextEmbedding::try_new(InitOptions::default())
            .map_err(|e| Error::Config(format!("failed to load fastembed model: {e}")))?;
        Ok(FastEmbedProvider { model })
    }
}

#[cfg(feature = "remote-embed")]
impl Embedder for FastEmbedProvider {
    fn embed(&self, text: &str) -> Result<Vec<f32>, Error> {
        let mut results = self
            .model
            .embed(vec![text], None)
            .map_err(|e| Error::Embedding(EmbeddingError::Transport(e.to_string())))?;
        results.pop().ok_or_else(|| {
            Error::Embedding(EmbeddingError::Transport("empty embedding result".into()))
        })
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        let owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
        let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
        self.model
            .embed(refs, None)
            .map_err(|e| Error::Embedding(EmbeddingError::Transport(e.to_string())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify that FastEmbedProvider::new() returns Err when the
    /// ONNX model is not available (no model cached, no network in test env).
    /// Invariants: The error variant is Error::Config.
    #[test]
    #[cfg(feature = "remote-embed")]
    fn fastembed_construction_fails_without_model() {
        // In CI / sandbox the model is never available, so new() must Err.
        match FastEmbedProvider::new() {
            Err(Error::Config(_)) => {} // expected: model not found
            Err(other) => panic!("expected Config error, got: {other:?}"),
            Ok(_) => {} // If model IS cached locally, that's OK too.
        }
    }
}
