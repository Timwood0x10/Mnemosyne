//! SSE (Server-Sent Events) transport for the MCP server.
//!
//! Implements the HTTP+SSE wire framing over an arbitrary byte stream
//! (`tokio::io::AsyncRead + AsyncWrite`): requests are delivered as SSE
//! `data:` events terminated by a blank line; responses are written the same
//! way. This keeps the transport testable over an in-memory duplex pair with
//! zero new HTTP dependencies; a concrete HTTP listener (axum/hyper route)
//! can wrap any `AsyncRead+AsyncWrite` pair and hand it to
//! [`SseTransport`] unchanged.
//!
//! Frame format (SSE spec + MCP HTTP+SSE):
//! - Send:   `data: {json}\n\n`
//! - Recv:   collect `data:` payload lines until a blank line, then parse the
//!   joined payload as one JSON-RPC message.

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::error::{Error, Result};
use crate::mcp::transport::Transport;
use crate::mcp::types::JSONRPCMessage;

/// SSE framing over a generic byte stream.
///
/// `S` is any `AsyncRead + AsyncWrite` pair (TCP stream, duplex test pair,
/// or an HTTP server's connection). Read and write sides share the stream.
pub struct SseTransport<S> {
    inner: BufReader<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> SseTransport<S> {
    /// Wrap a byte stream in SSE framing.
    #[must_use]
    pub fn new(stream: S) -> Self {
        Self {
            inner: BufReader::new(stream),
        }
    }

    /// Split ownership: convenience for tests that need independent halves.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.inner.into_inner()
    }
}

#[async_trait]
impl<S: AsyncRead + AsyncWrite + Unpin + Send> Transport for SseTransport<S> {
    async fn recv(&mut self) -> Result<Option<JSONRPCMessage>> {
        // Read SSE events: lines of `data: <payload>` until a blank line
        // terminates the event. A clean EOF before any data → None.
        let mut payload = String::new();
        let mut saw_event = false;
        loop {
            let mut line = String::new();
            let n = self
                .inner
                .read_line(&mut line)
                .await
                .map_err(|e| Error::Io(std::io::Error::other(format!("sse recv: {e}"))))?;
            if n == 0 {
                // EOF.
                if saw_event {
                    break; // we have a complete event; flush it
                }
                return Ok(None);
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                // Blank line terminates the event.
                if saw_event {
                    break;
                }
                continue;
            }
            if let Some(data) = trimmed.strip_prefix("data:") {
                let data = data.trim();
                if !data.is_empty() {
                    saw_event = true;
                    payload.push_str(data);
                }
            }
            // Other SSE fields (event:, id:) are ignored — JSON-RPC over MCP
            // HTTP+SSE only relies on `data`.
        }

        if payload.is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&payload)
            .map(Some)
            .map_err(|e| Error::JsonRpcParse(e.to_string()))
    }

    async fn send(&mut self, msg: &JSONRPCMessage) -> Result<()> {
        let json =
            serde_json::to_string(msg).map_err(|e| Error::Internal(format!("serialize: {e}")))?;
        let mut frame = String::with_capacity(json.len() + 8);
        frame.push_str("data: ");
        frame.push_str(&json);
        frame.push_str("\n\n");
        self.inner
            .write_all(frame.as_bytes())
            .await
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
        self.inner
            .flush()
            .await
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::types::{JSONRPCRequest, JSONRPCResponse};
    use serde_json::json;

    /// Round-trip a JSON-RPC request through a duplex pair.
    /// Invariants: the sender's message arrives byte-identical on the other
    /// side after SSE framing + unframing (compared field-by-field, since
    /// JSONRPCMessage has no PartialEq).
    #[tokio::test]
    async fn request_round_trips_over_duplex() {
        let (a, b) = tokio::io::duplex(1024);
        let mut sender = SseTransport::new(a);
        let mut receiver = SseTransport::new(b);

        let req = JSONRPCRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tools/list".into(),
            params: None,
        };
        sender
            .send(&JSONRPCMessage::Request(req.clone()))
            .await
            .expect("send");
        let got = receiver.recv().await.expect("recv");
        match got {
            Some(JSONRPCMessage::Request(r)) => {
                assert_eq!(r.jsonrpc, req.jsonrpc, "jsonrpc preserved");
                assert_eq!(r.id, req.id, "id preserved");
                assert_eq!(r.method, req.method, "method preserved");
                assert_eq!(r.params, req.params, "params preserved");
            }
            other => panic!("expected Request, got {other:?}"),
        }
    }

    /// Objective: Verify a response message round-trips.
    /// Invariants: response id/result preserved.
    #[tokio::test]
    async fn response_round_trips_over_duplex() {
        let (a, b) = tokio::io::duplex(1024);
        let mut sender = SseTransport::new(a);
        let mut receiver = SseTransport::new(b);

        let resp = JSONRPCResponse {
            jsonrpc: "2.0".into(),
            id: json!(7),
            result: Some(json!({"ok": true})),
            error: None,
        };
        sender
            .send(&JSONRPCMessage::Response(resp.clone()))
            .await
            .expect("send");
        let got = receiver.recv().await.expect("recv");
        match got {
            Some(JSONRPCMessage::Response(r)) => {
                assert_eq!(r.jsonrpc, resp.jsonrpc, "jsonrpc preserved");
                assert_eq!(r.id, resp.id, "id preserved");
                assert_eq!(r.result, resp.result, "result preserved");
                // Both sides carry no error in this test; JSONRPCError has no
                // PartialEq, so compare presence rather than value.
                assert!(
                    r.error.is_none() && resp.error.is_none(),
                    "error must stay None, got {r:?}"
                );
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    /// Objective: Verify clean EOF yields None (no hang, no panic).
    /// Invariants: dropping the writer then recv → Ok(None).
    #[tokio::test]
    async fn eof_returns_none() {
        let (a, b) = tokio::io::duplex(1024);
        let mut reader = SseTransport::new(b);
        drop(a); // writer gone → EOF on the read side
        let got = reader.recv().await.expect("recv");
        assert!(got.is_none(), "clean EOF must yield None, got {got:?}");
    }

    /// Objective: Verify malformed JSON payload surfaces JsonRpcParse (typed
    /// error, matching the server's -32700 handling).
    /// Invariants: `data: not-json` + blank line → Err(JsonRpcParse).
    #[tokio::test]
    async fn malformed_payload_is_typed_error() {
        use tokio::io::AsyncWriteExt;
        let (mut a, b) = tokio::io::duplex(1024);
        let mut reader = SseTransport::new(b);
        a.write_all(b"data: {not json}\n\n").await.expect("write");
        a.flush().await.expect("flush");
        let err = reader.recv().await.expect_err("must error");
        assert!(
            matches!(err, Error::JsonRpcParse(_)),
            "malformed SSE payload → JsonRpcParse, got {err:?}"
        );
    }
}
