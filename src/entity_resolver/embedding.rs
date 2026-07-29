//! Embedder trait — abstract text-to-vector interface.
//!
//! The entity resolution engine depends only on this trait, never on a specific
//! embedding model. Providers (FastEmbed, BGE, Jina, E5) swap out below this
//! trait without touching any resolver code.

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
