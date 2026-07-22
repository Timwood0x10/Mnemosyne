//! Unified error types for the memory distillation server.
//!
//! Library code returns [`Error`] values via `thiserror`; the binary entry
//! point converts them to user-facing messages with `anyhow`.

use thiserror::Error;

/// Top-level error enumeration.
///
/// Each variant corresponds to a distinct failure mode in the pipeline so
/// that callers can match on the category instead of inspecting strings.
#[derive(Debug, Error)]
pub enum Error {
    /// An embedding service request failed (network, decode, upstream 5xx).
    #[error("embedding service error: {0}")]
    Embedding(#[from] EmbeddingError),

    /// A storage backend operation failed (SQLite, vec index, schema).
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    /// The distillation pipeline failed at a specific phase.
    #[error("distillation error at phase `{phase}`: {message}")]
    Distillation { phase: String, message: String },

    /// Configuration loading or validation failed.
    #[error("config error: {0}")]
    Config(String),

    /// Input validation failure (bad MCP tool args, malformed message).
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// A required resource was not found by id.
    #[error("not found: {0}")]
    NotFound(String),

    /// A catch-all for errors that don't fit a specific category.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Errors from the embedding service.
#[derive(Debug, Error)]
pub enum EmbeddingError {
    /// Network or transport-level failure.
    #[error("transport error: {0}")]
    Transport(String),

    /// Upstream returned non-2xx status.
    #[error("upstream returned status {status}: {body}")]
    UpstreamStatus { status: u16, body: String },

    /// Response body could not be decoded.
    #[error("decode error: {0}")]
    Decode(String),

    /// Service returned empty embedding for non-empty input.
    #[error("empty embedding for non-empty input")]
    EmptyEmbedding,

    /// Health check failed.
    #[error("health check failed: {0}")]
    HealthCheckFailed(String),
}

/// Errors from the storage backend.
#[derive(Debug, Error)]
pub enum StorageError {
    /// SQLite or vec extension returned an error.
    #[error("sqlite error: {0}")]
    Sqlite(String),

    /// A migration or schema operation failed.
    #[error("schema error: {0}")]
    Schema(String),

    /// A record was not found by id.
    #[error("record not found: {0}")]
    NotFound(String),

    /// Vector dimensionality mismatch on insert/search.
    #[error("vector dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
}

impl From<rusqlite::Error> for Error {
    fn from(err: rusqlite::Error) -> Self {
        Error::Storage(StorageError::Sqlite(err.to_string()))
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::Internal(format!("json: {err}"))
    }
}

impl From<std::num::ParseFloatError> for Error {
    fn from(err: std::num::ParseFloatError) -> Self {
        Error::InvalidInput(format!("parse float: {err}"))
    }
}

/// Convenience type alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Build a distillation-phase error.
#[must_use]
pub fn distillation_error(phase: impl Into<String>, message: impl Into<String>) -> Error {
    Error::Distillation {
        phase: phase.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify Display output contains contextual phase info.
    /// Invariants: Error::Distillation display string contains both phase and message.
    #[test]
    fn distillation_error_display() {
        let err = distillation_error("extract", "no messages");
        let s = err.to_string();
        assert!(s.contains("extract"), "display should mention the phase");
        assert!(
            s.contains("no messages"),
            "display should mention the message"
        );
    }

    /// Objective: Verify rusqlite::Error converts to Storage variant.
    /// Invariants: Conversion preserves the SQL error message.
    #[test]
    fn sqlite_error_converts_to_storage() {
        let sql_err = rusqlite::Error::InvalidColumnIndex(7);
        let err: Error = sql_err.into();
        match err {
            Error::Storage(StorageError::Sqlite(msg)) => {
                assert!(msg.contains("7"), "message should echo the bad index");
            }
            other => panic!("expected Storage variant, got {other:?}"),
        }
    }

    /// Objective: Verify DimensionMismatch error preserves both numbers.
    /// Invariants: Display string contains expected and actual dimensions.
    #[test]
    fn dimension_mismatch_display() {
        let err = StorageError::DimensionMismatch {
            expected: 1024,
            actual: 768,
        };
        let s = err.to_string();
        assert!(
            s.contains("1024") && s.contains("768"),
            "display should show both dims"
        );
    }

    /// Objective: Verify EmbeddingError::UpstreamStatus carries body context.
    /// Invariants: Display string contains both status code and body.
    #[test]
    fn upstream_status_includes_body() {
        let err = EmbeddingError::UpstreamStatus {
            status: 503,
            body: "service unavailable".to_string(),
        };
        let s = err.to_string();
        assert!(s.contains("503"), "should mention the status code");
        assert!(s.contains("service unavailable"), "should mention the body");
    }
}
