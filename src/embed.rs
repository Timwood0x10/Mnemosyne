//! Embedding service trait and remote HTTP implementation.
//!
//! [`EmbeddingService`] mirrors the source project's interface: a stateless
//! service that converts text into a fixed-dimension `f32` vector. The remote
//! implementation talks to an upstream embedding-mcp server over HTTP.
//!
//! The trait is async-aware (`async fn`) so that callers can decide whether
//! to run concurrently (e.g. via `futures::join_all`) or sequentially.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{EmbeddingError, Result};

/// Async embedding service contract.
///
/// All methods return [`Result`]; callers propagate errors via `?`.
#[async_trait]
pub trait EmbeddingService: Send + Sync {
    /// Embed a single text into a vector.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Embed text with a prefix (e.g. `"query: "` for asymmetric retrieval).
    async fn embed_with_prefix(&self, text: &str, prefix: &str) -> Result<Vec<f32>>;

    /// Embed a batch of texts concurrently.
    ///
    /// Default implementation iterates sequentially; concrete services
    /// should override this with a batched HTTP call when possible.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed(t).await?);
        }
        Ok(out)
    }

    /// Lightweight health check used by the MCP `health` tool.
    async fn health_check(&self) -> Result<()>;

    /// Identifier of the underlying model (e.g. `"e5-large"`).
    fn model(&self) -> &str;

    /// Per-request timeout for the underlying transport.
    fn timeout(&self) -> std::time::Duration;

    /// Returns `true` when this service actually produces embeddings.
    ///
    /// `NullEmbedder` returns `false` so the retrieval layer can fall back
    /// to keyword search per `improve.md` Principle 1.
    fn enabled(&self) -> bool {
        true
    }
}

/// No-op embedder for the `embedding_provider = none` case.
///
/// Per `improve.md` Principle 1, the server must run without any embedding
/// backend. `NullEmbedder` returns empty vectors and reports `enabled() ==
/// false`, so callers can short-circuit embedding work and the retrieval
/// layer falls back to keyword search.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullEmbedder;

impl NullEmbedder {
    /// Construct a new null embedder.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl EmbeddingService for NullEmbedder {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(Vec::new())
    }

    async fn embed_with_prefix(&self, text: &str, _prefix: &str) -> Result<Vec<f32>> {
        self.embed(text).await
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| Vec::new()).collect())
    }

    async fn health_check(&self) -> Result<()> {
        Ok(())
    }

    fn model(&self) -> &str {
        "null"
    }

    fn timeout(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }

    fn enabled(&self) -> bool {
        false
    }
}

/// Request body for the upstream `/embed` endpoint.
#[derive(Debug, Serialize)]
struct EmbedRequest<'a> {
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    prefix: Option<&'a str>,
}

/// Response body from the upstream `/embed` endpoint.
#[derive(Debug, Deserialize)]
struct EmbedResponse {
    embedding: Vec<f32>,
}

/// HTTP-based remote embedder.
///
/// Talks to an upstream embedding-mcp server with a `POST /embed` endpoint
/// accepting JSON `{"text": "...", "prefix": "..."}` and returning
/// `{"embedding": [f32, ...]}`.
pub struct RemoteEmbedder {
    client: reqwest::Client,
    base_url: String,
    model: String,
    timeout: std::time::Duration,
}

impl RemoteEmbedder {
    /// Build a new remote embedder.
    ///
    /// # Arguments
    ///
    /// * `base_url` - Upstream server base URL (no trailing slash).
    /// * `model` - Model identifier, sent in the request header `X-Model`.
    /// * `timeout` - Per-request timeout.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Embedding`] if the HTTP client cannot be constructed.
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        timeout: std::time::Duration,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| EmbeddingError::Transport(e.to_string()))?;
        Ok(Self {
            client,
            base_url: base_url.into(),
            model: model.into(),
            timeout,
        })
    }
    /// Issue a single embed request and parse the response.
    async fn do_embed(&self, text: &str, prefix: Option<&str>) -> Result<Vec<f32>> {
        let body = EmbedRequest { text, prefix };
        let url = format!("{}/embed", self.base_url);

        let resp = self
            .client
            .post(&url)
            .header("X-Model", &self.model)
            .json(&body)
            .send()
            .await
            .map_err(|e| EmbeddingError::Transport(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(EmbeddingError::UpstreamStatus {
                status: status.as_u16(),
                body: body_text,
            }
            .into());
        }

        let parsed: EmbedResponse = resp
            .json()
            .await
            .map_err(|e| EmbeddingError::Decode(e.to_string()))?;

        if !text.trim().is_empty() && parsed.embedding.is_empty() {
            return Err(EmbeddingError::EmptyEmbedding.into());
        }
        Ok(parsed.embedding)
    }
}

#[async_trait]
impl EmbeddingService for RemoteEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.do_embed(text, None).await
    }

    async fn embed_with_prefix(&self, text: &str, prefix: &str) -> Result<Vec<f32>> {
        self.do_embed(text, Some(prefix)).await
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        // Sequential to keep memory bounded; override with concurrent calls
        // when batched endpoint exists.
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.do_embed(t, None).await?);
        }
        Ok(out)
    }

    async fn health_check(&self) -> Result<()> {
        let url = format!("{}/health", self.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| EmbeddingError::HealthCheckFailed(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(
                EmbeddingError::HealthCheckFailed(format!("status {}", resp.status())).into(),
            );
        }
        Ok(())
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn timeout(&self) -> std::time::Duration {
        self.timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify RemoteEmbedder construction succeeds with valid inputs.
    /// Invariants: new() returns Ok and preserves model id.
    #[test]
    fn remote_embedder_constructs() {
        let e = RemoteEmbedder::new(
            "http://localhost:8000",
            "e5-large",
            std::time::Duration::from_secs(5),
        )
        .expect("construct");
        assert_eq!(e.model(), "e5-large");
        assert_eq!(e.timeout(), std::time::Duration::from_secs(5));
    }

    /// Objective: Verify construction with empty model id still succeeds.
    /// Invariants: Empty model is allowed (caller's responsibility).
    #[test]
    fn remote_embedder_empty_model() {
        let e = RemoteEmbedder::new(
            "http://localhost:8000",
            "",
            std::time::Duration::from_secs(5),
        )
        .expect("construct");
        assert_eq!(e.model(), "");
    }
}
