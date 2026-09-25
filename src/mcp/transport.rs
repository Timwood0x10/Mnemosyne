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

/// Cap on a single stdio JSON-RPC line, mirroring the HTTP body cap
/// (`MAX_BODY_BYTES` in `http_server.rs`). A client streaming one endless
/// line would otherwise grow the buffer without bound and exhaust memory.
const MAX_LINE_BYTES: usize = 1_000_000;

/// Extra bytes drained after an oversized line so the next `read_line` starts
/// at a real message boundary instead of mid-payload.
const MAX_DRAIN_BYTES: usize = 16 * 1024 * 1024;

/// Read one bounded line from `reader`.
///
/// - EOF → `Ok(None)`.
/// - A line longer than [`MAX_LINE_BYTES`] → the remainder of the line is
///   drained (up to [`MAX_DRAIN_BYTES`]) and `Err(InvalidData)` is returned
///   so the caller can surface a parse error without desynchronizing stdin.
/// - Otherwise → `Ok(Some(line))` (newline preserved; `trim` happens later
///   in [`classify`]).
fn read_bounded_line<R: std::io::BufRead>(reader: &mut R) -> std::io::Result<Option<String>> {
    let mut line = String::new();
    // Cap the first read so a runaway line cannot allocate unboundedly.
    let n = std::io::Read::take(&mut *reader, (MAX_LINE_BYTES + 1) as u64).read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    // Strip the trailing newline BEFORE the length check: `line.len()` used
    // to include `\n`/`\r\n`, so a legitimate message of exactly
    // MAX_LINE_BYTES content bytes was rejected (off-by-one).
    let content_len = line.trim_end_matches('\n').trim_end_matches('\r').len();
    if content_len > MAX_LINE_BYTES {
        // Drain the rest of this line so the next message is not read from
        // the middle of the oversized payload.
        let mut drained = 0usize;
        let mut chunk = String::new();
        while drained < MAX_DRAIN_BYTES {
            chunk.clear();
            let mut take = std::io::Read::take(&mut *reader, 64 * 1024);
            let m = take.read_line(&mut chunk)?;
            if m == 0 {
                break;
            }
            drained += m;
            if chunk.ends_with('\n') {
                break;
            }
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("line exceeds {MAX_LINE_BYTES} bytes"),
        ));
    }
    Ok(Some(line))
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
            // so a slow/blocked stdin never stalls the tokio runtime.
            let raw = tokio::task::spawn_blocking(|| -> std::io::Result<Option<String>> {
                read_bounded_line(&mut std::io::stdin().lock())
            })
            .await
            .map_err(|e| Error::Internal(format!("spawn_blocking: {e}")))?;
            match raw {
                Ok(raw) => match classify(raw)? {
                    LineOutcome::Eof => return Ok(None),
                    LineOutcome::Blank => continue,
                    LineOutcome::Message(msg) => return Ok(Some(msg)),
                },
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                    // Oversized line: surface as a parse error so `serve`
                    // replies -32700 and keeps the connection alive, instead
                    // of treating it as fatal I/O and tearing everything down.
                    return Err(Error::JsonRpcParse(e.to_string()));
                }
                Err(e) => return Err(Error::Internal(format!("read_line: {e}"))),
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

    /// Objective: Verify `read_bounded_line` caps a single line at
    /// MAX_LINE_BYTES — a runaway line is rejected with an error instead of
    /// allocating without bound (audit finding: unbounded `read_line`).
    /// Invariants: a normal line reads back intact; a line longer than the
    /// cap errors; empty input (EOF) yields None.
    #[test]
    fn read_bounded_line_caps_oversized_lines() {
        use std::io::Cursor;

        // Normal line: read back intact. `read_line` keeps the trailing
        // newline (trim happens later in `classify`), so compare trimmed.
        let mut normal =
            Cursor::new(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n".to_vec());
        let got = read_bounded_line(&mut normal).expect("normal line reads");
        assert_eq!(
            got.as_deref().map(str::trim),
            Some("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}"),
            "normal line preserved"
        );

        // Oversized line: must error, not OOM or truncate silently.
        let huge = vec![b'x'; MAX_LINE_BYTES + 100];
        let mut oversized = Cursor::new(huge);
        let err = read_bounded_line(&mut oversized).expect_err("oversized line must error");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("exceeds"),
            "error explains the cap, got {err}"
        );

        // Empty input → EOF → None.
        let mut empty = Cursor::new(Vec::new());
        assert_eq!(
            read_bounded_line(&mut empty).expect("empty reads"),
            None,
            "EOF yields None"
        );
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
