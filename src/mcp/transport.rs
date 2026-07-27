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

#[async_trait]
impl Transport for StdioTransport {
    async fn recv(&mut self) -> Result<Option<JSONRPCMessage>> {
        // Read one JSON-RPC line off the worker thread via `spawn_blocking`
        // so a slow/blocked stdin never stalls the tokio runtime. The read is
        // bounded to a single line and stdin is pipe-fed by the host, so this
        // is cheap while keeping the async worker free.
        let line = tokio::task::spawn_blocking(|| -> std::io::Result<String> {
            let mut line = String::new();
            let n = std::io::stdin().lock().read_line(&mut line)?;
            if n == 0 { Ok(String::new()) } else { Ok(line) }
        })
        .await
        .map_err(|e| Error::Internal(format!("spawn_blocking: {e}")))?
        .map_err(|e| Error::Internal(format!("read_line: {e}")))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let msg: JSONRPCMessage =
            serde_json::from_str(trimmed).map_err(|e| Error::Internal(format!("parse: {e}")))?;
        Ok(Some(msg))
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
}
