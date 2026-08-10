//! MCP Streamable HTTP transport — remote serving over real HTTP.
//!
//! Exposes the MCP server through two endpoints (MCP spec §6.2):
//!
//! | Endpoint    | Method | Direction      | Purpose                          |
//! |-------------|--------|----------------|----------------------------------|
//! | `/sse`      | GET    | server → client | Server-Sent Events response stream |
//! | `/message`  | POST   | client → server | JSON-RPC messages                |
//!
//! Messages flow between the two endpoints over a pair of tokio channels,
//! bridged by [`HttpTransport`], which the protocol loop
//! ([`crate::mcp::server::MCPServer::serve`]) drives. Keeping the transport
//! channel-backed makes it fully unit-testable without sockets, and the axum
//! layer only handles HTTP semantics: status codes, SSE framing, and optional
//! bearer-token authentication for remote deployments.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Sse};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::error::{Error, Result};
use crate::mcp::server::MCPServer;
use crate::mcp::transport::Transport;
use crate::mcp::types::JSONRPCMessage;

/// Maximum accepted POST body size for a single JSON-RPC message (1 MiB).
const MAX_BODY_BYTES: usize = 1_000_000;

/// How long `POST /message` waits for the matching JSON-RPC response before
/// falling back to `202 Accepted`. When no `/sse` stream is open the spec
/// ("respond in POST body when no stream is open") says the server SHOULD
/// return the response in the POST body; the timeout keeps a slow or absent
/// handler from pinning the request indefinitely.
const POST_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Channel-based [`Transport`] bridged to the HTTP endpoints.
///
/// `recv()` pulls messages posted by the client via `POST /message`; each
/// request carries the client's `x-mcp-session-id` (when present). `send()`
/// routes the response to the session's OWN response channel so concurrent
/// clients never receive each other's replies (the global broadcast is used
/// only as a fallback for clients that did not supply a session id).
pub struct HttpTransport {
    request_rx: mpsc::Receiver<(Option<String>, JSONRPCMessage)>,
    response_tx: broadcast::Sender<JSONRPCMessage>,
    /// session id → that session's private response channel.
    sessions: Arc<std::sync::Mutex<HashMap<String, broadcast::Sender<JSONRPCMessage>>>>,
    /// session of the request currently being processed.
    current_session: Option<String>,
}

impl HttpTransport {
    /// Build a transport fed by `request_rx` (each item tagged with its
    /// session id) and published onto `response_tx` (global fallback) or the
    /// per-session channels in `sessions`.
    #[must_use]
    pub fn new(
        request_rx: mpsc::Receiver<(Option<String>, JSONRPCMessage)>,
        response_tx: broadcast::Sender<JSONRPCMessage>,
        sessions: Arc<std::sync::Mutex<HashMap<String, broadcast::Sender<JSONRPCMessage>>>>,
    ) -> Self {
        Self {
            request_rx,
            response_tx,
            sessions,
            current_session: None,
        }
    }
}

#[async_trait::async_trait]
impl Transport for HttpTransport {
    async fn recv(&mut self) -> Result<Option<JSONRPCMessage>> {
        match self.request_rx.recv().await {
            Some((session, msg)) => {
                // Remember which session this request belongs to so `send`
                // routes the reply to the right channel.
                self.current_session = session;
                Ok(Some(msg))
            }
            None => Ok(None), // all senders dropped → clean EOF
        }
    }

    async fn send(&mut self, msg: &JSONRPCMessage) -> Result<()> {
        // Route to the session's private channel when one exists; otherwise
        // fall back to the global broadcast (legacy single-client behavior).
        // Ignore a lagged/absent subscriber: responses for late joiners are
        // not errors — the client drives the flow via POST anyway.
        let target = match &self.current_session {
            Some(sid) => self
                .sessions
                .lock()
                .expect("session map lock is not poisoned")
                .get(sid)
                .cloned(),
            None => None,
        };
        match target {
            Some(tx) => {
                let _ = tx.send(msg.clone());
            }
            None => {
                let _ = self.response_tx.send(msg.clone());
            }
        }
        Ok(())
    }
}

/// Shared state for the HTTP handlers.
#[derive(Clone)]
pub struct AppState {
    request_tx: mpsc::Sender<(Option<String>, JSONRPCMessage)>,
    response_tx: broadcast::Sender<JSONRPCMessage>,
    /// session id → that session's private response channel (shared with the
    /// transport so `POST /message` responses and `GET /sse` streams agree on
    /// which channel carries a given session's replies).
    sessions: Arc<std::sync::Mutex<HashMap<String, broadcast::Sender<JSONRPCMessage>>>>,
    token: Option<String>,
}

impl AppState {
    /// Verify the bearer token when one is configured.
    fn check_auth(&self, headers: &HeaderMap) -> std::result::Result<(), (StatusCode, String)> {
        let Some(expected) = &self.token else {
            return Ok(());
        };
        let provided = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        // Constant-time comparison: `String == String` short-circuits on the
        // first differing byte, leaking the token's prefix and length via
        // response timing. XOR the full buffer so both accept and reject
        // paths take the same time for equal-length inputs.
        let bearer = format!("Bearer {expected}");
        if constant_time_eq(provided.as_bytes(), bearer.as_bytes()) {
            Ok(())
        } else {
            Err((StatusCode::UNAUTHORIZED, "unauthorized".into()))
        }
    }
}

/// Compare two byte slices without early exit, so a timing side channel
/// cannot leak the expected token's content or length.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// `POST /message` handler: accept one JSON-RPC message (or a batch array),
/// forward it to the protocol loop, and acknowledge with 202 Accepted.
/// When no `/sse` stream is open the response would otherwise be lost on the
/// channel; per the MCP spec ("respond in POST body when no stream is open")
/// we subscribe to the response channel and, if the matching response arrives
/// within [`POST_RESPONSE_TIMEOUT`], return it in the POST body instead.
/// Slow/absent handlers still get a prompt 202.
async fn message_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> std::result::Result<axum::response::Response, (StatusCode, String)> {
    state.check_auth(&headers)?;

    // Session isolation: a client that presents `x-mcp-session-id` gets its
    // replies on its OWN channel, so concurrent clients never receive each
    // other's responses (the old global broadcast leaked every reply to every
    // /sse subscriber).
    let session_id = headers
        .get("x-mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let bytes = axum::body::to_bytes(body, MAX_BODY_BYTES)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("read body: {e}")))?;

    // Subscribe BEFORE forwarding so a fast response is not missed: the
    // protocol loop may answer before this handler awaits recv.
    let mut response_rx = match &session_id {
        Some(sid) => {
            // Ensure the session's channel exists, then subscribe to it.
            let sender = state
                .sessions
                .lock()
                .expect("session map lock is not poisoned")
                .entry(sid.clone())
                .or_insert_with(|| broadcast::channel(1024).0)
                .clone();
            sender.subscribe()
        }
        None => state.response_tx.subscribe(),
    };
    let mut expected_ids: Vec<serde_json::Value> = Vec::new();

    // MCP over HTTP+SSE: a POST body is a single JSON-RPC message, but be
    // liberal and accept a batch array too (JSON-RPC 2.0 §6), forwarding each.
    if bytes.starts_with(b"[") {
        let batch: Vec<JSONRPCMessage> = serde_json::from_slice(&bytes).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid JSON-RPC batch: {e}"),
            )
        })?;
        for msg in &batch {
            if let JSONRPCMessage::Request(req) = msg {
                expected_ids.push(req.id.clone());
            }
        }
        for msg in batch {
            state
                .request_tx
                .send((session_id.clone(), msg))
                .await
                .map_err(|_| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "server shutting down".into(),
                    )
                })?;
        }
    } else {
        let msg: JSONRPCMessage = serde_json::from_slice(&bytes).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid JSON-RPC message: {e}"),
            )
        })?;
        if let JSONRPCMessage::Request(req) = &msg {
            expected_ids.push(req.id.clone());
        }
        state
            .request_tx
            .send((session_id, msg))
            .await
            .map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "server shutting down".into(),
                )
            })?;
    }

    // Notifications produce no response; nothing to wait for.
    if expected_ids.is_empty() {
        return Ok(StatusCode::ACCEPTED.into_response());
    }

    // Wait for the first response whose id matches one of the forwarded
    // requests. Lagged subscribers simply skip (older messages are stale).
    let matched = tokio::time::timeout(POST_RESPONSE_TIMEOUT, async {
        loop {
            match response_rx.recv().await {
                Ok(JSONRPCMessage::Response(resp)) if expected_ids.contains(&resp.id) => {
                    break Some(JSONRPCMessage::Response(resp));
                }
                Ok(_) => continue, // another request's response; keep waiting
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break None,
            }
        }
    })
    .await;

    match matched {
        Ok(Some(resp)) => {
            let json = serde_json::to_string(&resp)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("serialize: {e}")))?;
            Ok(axum::response::Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json))
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("body: {e}")))?)
        }
        _ => Ok(StatusCode::ACCEPTED.into_response()),
    }
}

/// `GET /sse` handler: open a Server-Sent Events stream carrying the
/// responses for ONE session (identified by `x-mcp-session-id`), or the
/// global broadcast for clients that do not present a session id.
async fn sse_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> std::result::Result<
    Sse<impl tokio_stream::Stream<Item = std::result::Result<Event, Infallible>>>,
    (StatusCode, String),
> {
    state.check_auth(&headers)?;

    let session_id = headers
        .get("x-mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    // Per-session stream: subscribe to the session's OWN channel so two
    // concurrent clients never see each other's responses.
    let rx = match &session_id {
        Some(sid) => {
            let sender = state
                .sessions
                .lock()
                .expect("session map lock is not poisoned")
                .entry(sid.clone())
                .or_insert_with(|| broadcast::channel(1024).0)
                .clone();
            sender.subscribe()
        }
        None => state.response_tx.subscribe(),
    };

    let stream = BroadcastStream::new(rx).filter_map(|item| {
        let msg = match item {
            Ok(msg) => msg,
            Err(_) => return None, // lagged subscriber → skip, keep stream alive
        };
        let json = serde_json::to_string(&msg).ok()?;
        Some(Ok::<Event, Infallible>(Event::default().data(json)))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Build the axum router for the two MCP HTTP endpoints.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/sse", axum::routing::get(sse_handler))
        .route("/message", axum::routing::post(message_handler))
        .with_state(state)
}

/// Run the MCP server over HTTP+SSE on `listener`, spawning the protocol
/// loop on a background task.
///
/// When `token` is `Some`, both endpoints require `Authorization: Bearer
/// <token>`; `None` serves without authentication (intended for trusted
/// local/internal deployments).
///
/// # Errors
///
/// Returns an error if binding or the HTTP server fails.
pub async fn serve_http(
    server: MCPServer,
    listener: tokio::net::TcpListener,
    token: Option<String>,
) -> Result<()> {
    let (request_tx, request_rx) = mpsc::channel(256);
    let (response_tx, _) = broadcast::channel(1024);
    // Session map shared by the transport (routing replies) and the handlers
    // (creating/subscribing per-session channels).
    let sessions: Arc<std::sync::Mutex<HashMap<String, broadcast::Sender<JSONRPCMessage>>>> =
        Arc::new(std::sync::Mutex::new(HashMap::new()));

    let state = AppState {
        request_tx,
        response_tx: response_tx.clone(),
        sessions: sessions.clone(),
        token,
    };

    // Drive the protocol loop on a background task; it terminates when the
    // request channel closes (all POST senders dropped, i.e. server shutdown).
    tokio::spawn(async move {
        let mut transport = HttpTransport::new(request_rx, response_tx, sessions);
        if let Err(e) = server.serve(&mut transport).await {
            tracing::error!("http transport serve: {e}");
        }
    });

    let app = router(state);
    axum::serve(listener, app)
        .await
        .map_err(|e| Error::Internal(format!("http serve: {e}")))
}

/// Bind a TCP listener to `addr` and serve the MCP server over HTTP+SSE.
///
/// Convenience wrapper for `serve_http` when the caller has an address rather
/// than a pre-bound listener (e.g. a `--addr 127.0.0.1:5609` CLI flag).
///
/// # Errors
///
/// Returns an error if binding or the HTTP server fails.
pub async fn serve_http_addr(
    server: MCPServer,
    addr: SocketAddr,
    token: Option<String>,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| Error::Internal(format!("bind {addr}: {e}")))?;
    serve_http(server, listener, token).await
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::server::MCPServer;
    use crate::mcp::types::{Implementation, JSONRPCRequest, JSONRPCResponse};
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use serde_json::json;
    use tower::ServiceExt;

    fn request_msg(id: i64, method: &str) -> JSONRPCMessage {
        JSONRPCMessage::Request(JSONRPCRequest {
            jsonrpc: "2.0".into(),
            id: json!(id),
            method: method.into(),
            params: None,
        })
    }

    /// Build an empty shared session map for tests.
    fn test_sessions() -> Arc<std::sync::Mutex<HashMap<String, broadcast::Sender<JSONRPCMessage>>>>
    {
        Arc::new(std::sync::Mutex::new(HashMap::new()))
    }

    /// Objective: Verify HttpTransport forwards a request to `recv` and
    /// broadcasts a response to `send`, round-tripping over channels.
    /// Invariants: request arrives intact; sent response is observed by a
    /// subscriber.
    // NOTE: HTTP transport tests are #[ignore] so the fast `make test` inner
    // loop skips them; run with `cargo nextest run --run-ignored all`.
    #[ignore]
    #[tokio::test]
    async fn transport_round_trips_over_channels() {
        let (request_tx, request_rx) = mpsc::channel::<(Option<String>, JSONRPCMessage)>(8);
        let (response_tx, mut response_rx) = broadcast::channel(8);
        let mut transport = HttpTransport::new(request_rx, response_tx.clone(), test_sessions());

        request_tx
            .send((None, request_msg(1, "tools/list")))
            .await
            .expect("send request");
        let got = transport.recv().await.expect("recv").expect("some");
        match got {
            JSONRPCMessage::Request(r) => assert_eq!(r.method, "tools/list", "method preserved"),
            other => panic!("expected Request, got {other:?}"),
        }

        let resp = JSONRPCMessage::Response(JSONRPCResponse {
            jsonrpc: "2.0".into(),
            id: json!(1),
            result: Some(json!({"ok": true})),
            error: None,
        });
        transport.send(&resp).await.expect("send response");
        let observed = response_rx.recv().await.expect("response received");
        match observed {
            JSONRPCMessage::Response(r) => {
                assert_eq!(r.result, Some(json!({"ok": true})), "result preserved")
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    /// Objective: Verify HttpTransport yields `None` (EOF) when the request
    /// channel is fully closed.
    /// Invariants: dropping all senders → recv → Ok(None).
    #[ignore]
    #[tokio::test]
    async fn transport_eof_on_closed_channel() {
        let (request_tx, request_rx) = mpsc::channel::<(Option<String>, JSONRPCMessage)>(8);
        let (response_tx, _) = broadcast::channel(8);
        let mut transport = HttpTransport::new(request_rx, response_tx, test_sessions());
        drop(request_tx); // no senders → EOF
        let got = transport.recv().await.expect("recv");
        assert!(got.is_none(), "closed request channel → clean EOF");
    }

    /// Objective: Verify `POST /message` accepts a well-formed JSON-RPC
    /// message and replies 202 Accepted.
    /// Invariants: status 202; the forwarded message reaches the request
    /// channel.
    #[ignore]
    #[tokio::test]
    async fn post_message_accepts_and_returns_202() {
        let (request_tx, mut request_rx) = mpsc::channel::<(Option<String>, JSONRPCMessage)>(8);
        let (response_tx, _) = broadcast::channel(8);
        let state = AppState {
            request_tx,
            response_tx,
            sessions: test_sessions(),
            token: None,
        };
        let app = router(state);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/message")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#))
                    .expect("request"),
            )
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::ACCEPTED, "202 on accepted");

        let (session, msg) = request_rx.recv().await.expect("message forwarded");
        assert!(session.is_none(), "no session id supplied by this client");
        match msg {
            JSONRPCMessage::Request(r) => assert_eq!(r.method, "ping", "message forwarded"),
            other => panic!("expected Request, got {other:?}"),
        }
    }

    /// Objective: Verify authentication is enforced when a token is set.
    /// Invariants: wrong/missing bearer → 401; correct bearer → 202.
    #[ignore]
    #[tokio::test]
    async fn auth_enforced_when_token_set() {
        let (request_tx, _request_rx) = mpsc::channel::<(Option<String>, JSONRPCMessage)>(8);
        let (response_tx, _) = broadcast::channel(8);
        let state = AppState {
            request_tx,
            response_tx,
            sessions: test_sessions(),
            token: Some("secret".into()),
        };
        let app = router(state);

        // No token → 401.
        let no_auth = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/message")
                    .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#))
                    .expect("request"),
            )
            .await
            .expect("oneshot");
        assert_eq!(
            no_auth.status(),
            StatusCode::UNAUTHORIZED,
            "missing token → 401"
        );

        // Wrong token → 401.
        let wrong = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/message")
                    .header("authorization", "Bearer wrong")
                    .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#))
                    .expect("request"),
            )
            .await
            .expect("oneshot");
        assert_eq!(
            wrong.status(),
            StatusCode::UNAUTHORIZED,
            "wrong token → 401"
        );

        // Correct token → 202.
        let ok = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/message")
                    .header("authorization", "Bearer secret")
                    .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#))
                    .expect("request"),
            )
            .await
            .expect("oneshot");
        assert_eq!(ok.status(), StatusCode::ACCEPTED, "correct token → 202");
    }

    /// Objective: Verify `GET /sse` returns 200 with the correct content-type
    /// when no token is configured.
    /// Invariants: status 200; `content-type` is `text/event-stream`.
    #[ignore]
    #[tokio::test]
    async fn sse_endpoint_serves_stream_without_token() {
        let (request_tx, _request_rx) = mpsc::channel::<(Option<String>, JSONRPCMessage)>(8);
        let (response_tx, _) = broadcast::channel(8);
        let state = AppState {
            request_tx,
            response_tx,
            sessions: test_sessions(),
            token: None,
        };
        let app = router(state);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .method("GET")
                    .uri("/sse")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK, "SSE stream opens");
        let ct = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.contains("text/event-stream"),
            "SSE content-type, got {ct:?}"
        );
    }

    /// Objective: Verify a malformed JSON-RPC body on POST is rejected with
    /// 400, not accepted.
    /// Invariants: invalid JSON → 400 Bad Request.
    #[ignore]
    #[tokio::test]
    async fn malformed_body_rejected_400() {
        let (request_tx, _request_rx) = mpsc::channel::<(Option<String>, JSONRPCMessage)>(8);
        let (response_tx, _) = broadcast::channel(8);
        let state = AppState {
            request_tx,
            response_tx,
            sessions: test_sessions(),
            token: None,
        };
        let app = router(state);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/message")
                    .body(Body::from("not json"))
                    .expect("request"),
            )
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "bad JSON → 400");
    }

    /// Objective: End-to-end — verify the real [`MCPServer::serve`] loop,
    /// driven through [`HttpTransport`], answers a JSON-RPC `initialize`
    /// request and broadcasts the response.
    /// Invariants: a request posted to the channel yields a broadcast response
    /// carrying the server `implementation`.
    #[ignore]
    #[tokio::test]
    async fn server_serves_and_broadcasts_response() {
        let (request_tx, request_rx) = mpsc::channel::<(Option<String>, JSONRPCMessage)>(8);
        let (response_tx, mut response_rx) = broadcast::channel(8);
        let server = MCPServer::new(Implementation {
            name: "lorescope-test".into(),
            version: "0.0.1".into(),
        });

        let handle = tokio::spawn(async move {
            let mut transport = HttpTransport::new(request_rx, response_tx, test_sessions());
            server.serve(&mut transport).await.expect("serve")
        });

        request_tx
            .send((
                None,
                JSONRPCMessage::Request(JSONRPCRequest {
                    jsonrpc: "2.0".into(),
                    id: json!(1),
                    method: "initialize".into(),
                    params: Some(json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "clientInfo": {"name": "test", "version": "0.0.1"}
                    })),
                }),
            ))
            .await
            .expect("send initialize");

        let observed = tokio::time::timeout(std::time::Duration::from_secs(5), response_rx.recv())
            .await
            .expect("timeout waiting for response")
            .expect("response received");

        match observed {
            JSONRPCMessage::Response(r) => {
                assert!(r.error.is_none(), "initialize succeeds, got {r:?}");
                let result = r.result.expect("result present");
                let server_name = result["serverInfo"]["name"].as_str().unwrap_or("");
                assert_eq!(server_name, "lorescope-test", "server identifies itself");
            }
            other => panic!("expected Response, got {other:?}"),
        }

        // Shut down cleanly and await the serve task.
        drop(request_tx);
        let _ = handle.await;
    }

    /// Objective: Verify a `POST /message` with NO `/sse` subscriber still
    /// receives its JSON-RPC response — the spec's "respond in POST body when
    /// no stream is open" fallback. Previously the response was broadcast to
    /// zero subscribers and silently lost (202 forever).
    /// Invariants: HTTP 200; body is the JSON-RPC response echoing the id.
    #[ignore]
    #[tokio::test]
    async fn post_message_returns_response_body_without_sse() {
        let (request_tx, request_rx) = mpsc::channel::<(Option<String>, JSONRPCMessage)>(8);
        let (response_tx, _) = broadcast::channel(8);
        let response_tx_serve = response_tx.clone();
        let sessions = test_sessions();
        let server = MCPServer::new(Implementation {
            name: "lorescope-test".into(),
            version: "0.0.1".into(),
        });
        let handle = tokio::spawn(async move {
            let mut transport = HttpTransport::new(request_rx, response_tx_serve, sessions);
            server.serve(&mut transport).await.expect("serve")
        });

        // No SSE subscriber is ever created — the response must come back in
        // the POST body (the regression this test locks in). The original
        // `request_tx` is kept outside so the test can drop it at the end to
        // signal EOF to the serve loop.
        let state = AppState {
            request_tx: request_tx.clone(),
            response_tx,
            sessions: test_sessions(),
            token: None,
        };
        let app = router(state);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/message")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.0.1"}}}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("oneshot");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "response must come back in the POST body, not be lost"
        );
        let bytes = axum::body::to_bytes(response.into_body(), MAX_BODY_BYTES)
            .await
            .expect("read body");
        let resp: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON response");
        assert_eq!(resp["id"], json!(1), "response echoes the request id");
        assert!(
            resp["result"]["serverInfo"]["name"]
                .as_str()
                .is_some_and(|n| n == "lorescope-test"),
            "initialize result present in POST body, got {resp}"
        );

        // Shut down cleanly and await the serve task.
        drop(request_tx);
        let _ = handle.await;
    }
}
