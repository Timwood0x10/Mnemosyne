//! Embedding service trait and remote HTTP implementation.
//!
//! [`EmbeddingService`] mirrors the source project's interface: a stateless
//! service that converts text into a fixed-dimension `f32` vector. The remote
//! implementation talks to an upstream embedding-mcp server over HTTP.
//!
//! The trait is async-aware (`async fn`) so that callers can decide whether
//! to run concurrently (e.g. via `futures::join_all`) or sequentially.

use async_trait::async_trait;
#[cfg(feature = "remote-embed")]
use serde::{Deserialize, Serialize};

#[cfg(feature = "remote-embed")]
use crate::error::EmbeddingError;
use crate::error::Result;

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
#[cfg(feature = "remote-embed")]
#[derive(Debug, Serialize)]
struct EmbedRequest<'a> {
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    prefix: Option<&'a str>,
}

/// Response body from the upstream `/embed` endpoint.
#[cfg(feature = "remote-embed")]
#[derive(Debug, Deserialize)]
struct EmbedResponse {
    embedding: Vec<f32>,
}

/// HTTP-based remote embedder.
///
/// Talks to an upstream embedding-mcp server with a `POST /embed` endpoint
/// accepting JSON `{"text": "...", "prefix": "..."}` and returning
/// `{"embedding": [f32, ...]}`.
#[cfg(feature = "remote-embed")]
pub struct RemoteEmbedder {
    client: reqwest::Client,
    base_url: String,
    model: String,
    timeout: std::time::Duration,
    /// Bearer token sent as `Authorization` when the upstream requires one.
    /// `None` (the default) sends no auth header, so keyless local servers
    /// keep working unchanged.
    api_key: Option<String>,
}

#[cfg(feature = "remote-embed")]
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
            api_key: None,
        })
    }

    /// Attach the bearer token used against an authenticated upstream.
    ///
    /// Without this the configured `openai_api_key` would be validated at
    /// startup and then silently dropped, so an operator setting it got an
    /// anonymous request.
    #[must_use]
    pub fn with_api_key(mut self, api_key: Option<String>) -> Self {
        self.api_key = api_key;
        self
    }

    /// Issue a single embed request and parse the response.
    async fn do_embed(&self, text: &str, prefix: Option<&str>) -> Result<Vec<f32>> {
        let body = EmbedRequest { text, prefix };
        let url = format!("{}/embed", self.base_url);

        let mut request = self.client.post(&url).header("X-Model", &self.model);
        if let Some(api_key) = &self.api_key {
            request = request.bearer_auth(api_key);
        }
        let resp = request
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

#[cfg(feature = "remote-embed")]
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

#[cfg(all(test, feature = "remote-embed"))]
mod tests {
    use super::*;

    /// Objective: Verify RemoteEmbedder construction succeeds with valid inputs.
    /// Invariants: new() returns Ok and preserves model id.
    #[test]
    #[cfg(feature = "remote-embed")]
    fn remote_embedder_constructs() {
        let e = RemoteEmbedder::new(
            "http://localhost:8000",
            "e5-large",
            std::time::Duration::from_secs(5),
        )
        .expect("construct");
        assert_eq!(e.model(), "e5-large", "the model id must round-trip");
        assert_eq!(
            e.timeout(),
            std::time::Duration::from_secs(5),
            "the timeout must round-trip"
        );
    }

    /// Objective: Verify construction with empty model id still succeeds.
    /// Invariants: Empty model is allowed (caller's responsibility).
    #[test]
    #[cfg(feature = "remote-embed")]
    fn remote_embedder_empty_model() {
        let e = RemoteEmbedder::new(
            "http://localhost:8000",
            "",
            std::time::Duration::from_secs(5),
        )
        .expect("construct");
        assert_eq!(e.model(), "", "an empty model id must be preserved");
    }

    /// Objective: Verify a configured API key reaches the wire as
    /// `Authorization: Bearer`, so the key the config layer validates is
    /// actually used instead of being silently dropped.
    /// Invariants: with a key the request carries the bearer token and the
    /// `X-Model` id; without one, no `Authorization` header is sent at all.
    #[tokio::test]
    #[cfg(feature = "remote-embed")]
    async fn api_key_is_sent_as_bearer_authorization() {
        for (label, key) in [
            ("with api key", Some("sk-test".to_string())),
            ("keyless upstream", None),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind listener");
            let addr = listener.local_addr().expect("listener address");
            let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
            let sink = std::sync::Arc::clone(&captured);
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept request");
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .expect("set read timeout");
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                loop {
                    let n = std::io::Read::read(&mut stream, &mut buf).expect("read request");
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..n]);
                    // Headers are complete once the blank line arrives.
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                *sink.lock().expect("capture lock") =
                    String::from_utf8_lossy(&request).into_owned();
                let body = r#"{"embedding":[1.0,2.0]}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                std::io::Write::write_all(&mut stream, response.as_bytes())
                    .expect("write response");
            });

            let embedder = RemoteEmbedder::new(
                format!("http://{addr}"),
                "e5-large",
                std::time::Duration::from_secs(5),
            )
            .expect("construct")
            .with_api_key(key.clone());

            let vector = embedder.embed("hello").await.expect("embed request");
            server.join().expect("join server thread");
            assert_eq!(
                vector,
                vec![1.0, 2.0],
                "{label}: the response body must be parsed"
            );

            let request = captured.lock().expect("capture lock").clone();
            // HTTP/1 header names are case-insensitive, and reqwest emits them
            // lowercased, so compare on the folded form.
            let folded = request.to_ascii_lowercase();
            assert!(
                folded.contains("x-model: e5-large"),
                "{label}: the model header must always be sent, got:\n{request}"
            );
            match &key {
                Some(token) => assert!(
                    folded.contains(&format!("authorization: bearer {token}")),
                    "{label}: the configured key must be sent as a bearer token, got:\n{request}"
                ),
                None => assert!(
                    !folded.contains("authorization"),
                    "{label}: a keyless upstream must receive no auth header, got:\n{request}"
                ),
            }
        }
    }
}
