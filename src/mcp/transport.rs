//! Transport layer for the MCP server.
//!
//! [`Transport`] is the trait surface; [`StdioTransport`] is the concrete
//! implementation that reads JSON-RPC messages from stdin and writes
//! responses to stdout, one message per line.

use async_trait::async_trait;
use std::io::{BufRead, Write};

use crate::error::{Error, Result};
use crate::mcp::types::JSONRPCMessage;

/// Async transport contract for the MCP server.
///
/// The server polls [`Transport::recv`] in a loop, dispatches each
/// message to the appropriate handler, and writes responses back via
/// [`Transport::send`].
#[async_trait]
pub trait Transport: Send {
    /// Receive the next JSON-RPC message, or `None` on EOF.
    async fn recv(&mut self) -> Result<Option<JSONRPCMessage>>;

    /// Send a JSON-RPC message to the client.
    async fn send(&mut self, msg: &JSONRPCMessage) -> Result<()>;
}

/// stdio transport: reads line-delimited JSON from stdin, writes to stdout.
pub struct StdioTransport {
    stdout: std::io::Stdout,
}

impl StdioTransport {
    /// Build a new stdio transport wired to the process stdin/stdout.
    #[must_use]
    pub fn new() -> Self {
        Self {
            stdout: std::io::stdout(),
        }
    }
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of classifying one raw line read from the stdio channel.
#[derive(Debug)]
enum LineOutcome {
    /// EOF reached — the server should stop serving.
    Eof,
    /// Blank / whitespace-only framing line — ignore and keep reading.
    Blank,
    /// A JSON-RPC message to dispatch.
    Message(JSONRPCMessage),
}

/// Classify a single raw stdin line.
///
/// A blank/whitespace-only line is legal framing noise between messages and
/// must NOT terminate the server — treating it as EOF used to kill the whole
/// stdio server on a stray blank line. Only a true EOF (`None`) stops the loop.
fn classify(raw: Option<String>) -> Result<LineOutcome> {
    match raw {
        None => Ok(LineOutcome::Eof),
        Some(line) => {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                Ok(LineOutcome::Blank)
            } else {
                let msg: JSONRPCMessage = serde_json::from_str(trimmed)
                    .map_err(|e| Error::JsonRpcParse(e.to_string()))?;
                Ok(LineOutcome::Message(msg))
            }
        }
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn recv(&mut self) -> Result<Option<JSONRPCMessage>> {
        loop {
            // Read one JSON-RPC line off the worker thread via `spawn_blocking`
            // so a slow/blocked stdin never stalls the tokio runtime. The read is
            // bounded to a single line and stdin is pipe-fed by the host, so this
            // is cheap while keeping the async worker free.
            let raw = tokio::task::spawn_blocking(|| -> std::io::Result<Option<String>> {
                let mut line = String::new();
                let n = std::io::stdin().lock().read_line(&mut line)?;
                if n == 0 { Ok(None) } else { Ok(Some(line)) }
            })
            .await
            .map_err(|e| Error::Internal(format!("spawn_blocking: {e}")))?
            .map_err(|e| Error::Internal(format!("read_line: {e}")))?;
            match classify(raw)? {
                LineOutcome::Eof => return Ok(None),
                LineOutcome::Blank => continue,
                LineOutcome::Message(msg) => return Ok(Some(msg)),
            }
        }
    }

    async fn send(&mut self, msg: &JSONRPCMessage) -> Result<()> {
        let json =
            serde_json::to_string(msg).map_err(|e| Error::Internal(format!("serialize: {e}")))?;
        let mut out = self.stdout.lock();
        out.write_all(json.as_bytes())
            .map_err(|e| Error::Internal(format!("write: {e}")))?;
        out.write_all(b"\n")
            .map_err(|e| Error::Internal(format!("write newline: {e}")))?;
        out.flush()
            .map_err(|e| Error::Internal(format!("flush: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify StdioTransport::new constructs without panic.
    /// Invariants: new() returns a value, default() equals new().
    #[test]
    fn stdio_transport_constructs() {
        let _ = StdioTransport::new();
        let _ = StdioTransport::default();
    }

    /// Objective: Verify a blank/whitespace-only line yields Blank (skip),
    /// NOT EOF — a stray blank line must not kill the stdio server.
    /// Invariants: "", "  ", "\t\n" all classify as Blank.
    #[test]
    fn blank_line_is_skipped_not_eof() {
        for line in ["", "   ", "\t\n", " \n"] {
            assert!(
                matches!(
                    classify(Some(line.to_string())).expect("classify"),
                    LineOutcome::Blank
                ),
                "blank line {line:?} must be skipped, not treated as EOF"
            );
        }
    }

    /// Objective: Verify only a true EOF (None) stops the server.
    /// Invariants: classify(None) == Eof.
    #[test]
    fn eof_stops_server() {
        assert!(
            matches!(classify(None).expect("classify"), LineOutcome::Eof),
            "None must yield Eof"
        );
    }

    /// Objective: Verify a well-formed JSON-RPC request line parses into a
    /// Message.
    /// Invariants: the parsed request keeps its method and id.
    #[test]
    fn valid_message_parses() {
        let raw = r#"{"jsonrpc":"2.0","id":7,"method":"memory_stats","params":{}}"#;
        match classify(Some(raw.to_string())).expect("classify") {
            LineOutcome::Message(JSONRPCMessage::Request(req)) => {
                assert_eq!(req.method, "memory_stats", "method preserved");
                assert_eq!(req.id, serde_json::json!(7), "id preserved");
            }
            other => panic!("expected Message(Request), got {other:?}"),
        }
    }

    /// Objective: Verify a malformed JSON line surfaces a JsonRpcParse error
    /// instead of silently stopping or looping forever.
    /// Invariants: classify returns Err(JsonRpcParse) for invalid JSON.
    #[test]
    fn malformed_json_is_parse_error() {
        let err = classify(Some("not-json".to_string())).expect_err("must error");
        assert!(
            matches!(err, Error::JsonRpcParse(_)),
            "malformed line must yield JsonRpcParse, got {err:?}"
        );
    }

    /// Objective: Verify a JSON value that is not a message shape still
    /// attempts classification (surfacing a parse/type error rather than a
    /// silent drop).
    /// Invariants: a bare array is rejected (no Message outcome).
    #[test]
    fn non_message_json_is_rejected() {
        let raw = "[1,2,3]";
        let outcome = classify(Some(raw.to_string()));
        assert!(
            outcome.is_err(),
            "a JSON array is not a valid JSON-RPC message: {outcome:?}"
        );
    }
}
