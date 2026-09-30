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
use std::time::{Duration, Instant};

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

/// HTTP header carrying the session id.
///
/// The MCP spelling is checked first; the lowercase `x-mcp-session-id` this
/// server used to advertise is still read, so existing clients keep working.
const SESSION_HEADER: &str = "mcp-session-id";
const LEGACY_SESSION_HEADER: &str = "x-mcp-session-id";

/// How long a session may stay idle before its channel is dropped.
const SESSION_TTL: Duration = Duration::from_secs(30 * 60);

/// Per-session reply buffer, in messages. A subscriber that falls behind drops
/// the oldest replies (the client re-requests); it never blocks the server.
const SESSION_CHANNEL_CAPACITY: usize = 1024;

/// One client's private reply channel, plus when it was last used.
///
/// `pub(crate)` only because [`HttpTransport::new`] takes the map of them and is
/// itself `pub(crate)`; nothing outside the crate can name this type.
pub(crate) struct SessionEntry {
    sender: broadcast::Sender<JSONRPCMessage>,
    last_seen: Instant,
}

/// The sessions this server has **issued**, keyed by an unguessable id.
type SessionMap = Arc<std::sync::Mutex<HashMap<String, SessionEntry>>>;

/// Read the session id a client presented, if any.
fn session_header(headers: &HeaderMap) -> Option<String> {
    [SESSION_HEADER, LEGACY_SESSION_HEADER]
        .iter()
        .find_map(|name| headers.get(*name).and_then(|value| value.to_str().ok()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Mint a session id.
///
/// Unguessable on purpose: the server used to accept whatever string a client
/// sent (and never issued one), so presenting `"1"` or `"default"` was enough to
/// subscribe to another client's replies (audit C4).
fn mint_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Drop sessions idle for longer than [`SESSION_TTL`].
///
/// Nothing ever removed an entry when a client went away, so every disconnect
/// left a 1024-slot broadcast channel behind for the life of the process
/// (audit H8). Returns how many entries were dropped.
fn evict_stale_sessions(sessions: &mut HashMap<String, SessionEntry>, now: Instant) -> usize {
    let before = sessions.len();
    sessions.retain(|_, entry| now.duration_since(entry.last_seen) < SESSION_TTL);
    before - sessions.len()
}

/// Whether this POST body carries an `initialize` request.
///
/// Parsed leniently: anything unreadable is NOT an initialize, so it lands in the
/// "present a session" branch and is refused there — the client then sees the
/// session error rather than a JSON error, which is the order the protocol needs.
fn is_initialize(bytes: &[u8]) -> bool {
    #[derive(serde::Deserialize)]
    struct MethodOnly {
        method: Option<String>,
    }
    let is_init = |m: &MethodOnly| m.method.as_deref() == Some("initialize");
    if bytes.starts_with(b"[") {
        return serde_json::from_slice::<Vec<MethodOnly>>(bytes)
            .is_ok_and(|batch| batch.iter().any(is_init));
    }
    serde_json::from_slice::<MethodOnly>(bytes).is_ok_and(|m| is_init(&m))
}

/// Decide which session a request runs as, or refuse it.
///
/// Returns the session id and, when this call minted it, the id to echo back in
/// the response header.
///
/// # Errors
///
/// Returns `NOT_FOUND` for a session id this server did not issue, and
/// `BAD_REQUEST` when none was presented — except `initialize`, which is how a
/// session is obtained in the first place.
fn resolve_session(
    state: &AppState,
    presented: Option<String>,
    bytes: &[u8],
) -> std::result::Result<(String, Option<String>), (StatusCode, String)> {
    match presented {
        Some(session_id) if state.knows_session(&session_id) => Ok((session_id, None)),
        Some(session_id) => Err((
            StatusCode::NOT_FOUND,
            format!("unknown session `{session_id}`; POST `initialize` to obtain one"),
        )),
        None if is_initialize(bytes) => {
            let session_id = state.issue_session();
            Ok((session_id.clone(), Some(session_id)))
        }
        None => Err((
            StatusCode::BAD_REQUEST,
            format!("missing `{SESSION_HEADER}`; POST `initialize` to obtain one"),
        )),
    }
}

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
    /// session id → that session's private response channel.
    sessions: SessionMap,
    /// session of the request currently being processed.
    current_session: Option<String>,
}

impl HttpTransport {
    /// Build a transport fed by `request_rx`, each item tagged with the session
    /// it belongs to; replies go to that session's own channel.
    ///
    /// There is deliberately no global fallback: it existed to serve requests that
    /// named no session, which meant every `/sse` subscriber could observe every
    /// reply (audit C4).
    #[must_use]
    pub(crate) fn new(
        request_rx: mpsc::Receiver<(Option<String>, JSONRPCMessage)>,
        sessions: SessionMap,
    ) -> Self {
        Self {
            request_rx,
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
        // Route to the session's private channel. A lagged/absent subscriber is
        // not an error: the client drives the flow via POST anyway.
        let target = self.current_session.as_ref().and_then(|sid| {
            self.sessions
                .lock()
                .expect("session map lock is not poisoned")
                .get(sid)
                .map(|entry| entry.sender.clone())
        });
        match target {
            Some(tx) => {
                let _ = tx.send(msg.clone());
            }
            None => {
                // Nothing to route to. Broadcasting instead would hand this reply
                // to every `/sse` subscriber — the leak the session map exists to
                // prevent (audit C4) — so it is dropped and reported.
                tracing::warn!(
                    session = self.current_session.as_deref().unwrap_or("<none>"),
                    "dropping a response with no session channel to route it to"
                );
            }
        }
        Ok(())
    }
}

/// Shared state for the HTTP handlers.
#[derive(Clone)]
pub struct AppState {
    request_tx: mpsc::Sender<(Option<String>, JSONRPCMessage)>,
    /// Sessions this server issued (shared with the transport so `POST /message`
    /// responses and `GET /sse` streams agree on which channel carries a given
    /// session's replies).
    sessions: SessionMap,
    token: Option<String>,
}

impl AppState {
    /// Whether `session_id` is one this server issued and has not dropped.
    fn knows_session(&self, session_id: &str) -> bool {
        let mut sessions = self
            .sessions
            .lock()
            .expect("session map lock is not poisoned");
        evict_stale_sessions(&mut sessions, Instant::now());
        sessions.contains_key(session_id)
    }

    /// The session's channel, refreshing its idle timer.
    fn session_channel(&self, session_id: &str) -> broadcast::Sender<JSONRPCMessage> {
        let now = Instant::now();
        let mut sessions = self
            .sessions
            .lock()
            .expect("session map lock is not poisoned");
        evict_stale_sessions(&mut sessions, now);
        let entry = sessions
            .entry(session_id.to_string())
            .or_insert_with(|| SessionEntry {
                sender: broadcast::channel(SESSION_CHANNEL_CAPACITY).0,
                last_seen: now,
            });
        entry.last_seen = now;
        entry.sender.clone()
    }

    /// Issue a session id and register its channel.
    fn issue_session(&self) -> String {
        let session_id = mint_session_id();
        let now = Instant::now();
        let mut sessions = self
            .sessions
            .lock()
            .expect("session map lock is not poisoned");
        evict_stale_sessions(&mut sessions, now);
        sessions.insert(
            session_id.clone(),
            SessionEntry {
                sender: broadcast::channel(SESSION_CHANNEL_CAPACITY).0,
                last_seen: now,
            },
        );
        session_id
    }

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

/// Compare two byte slices without a length-based early exit, so a timing side
/// channel cannot leak the expected token's length (audit L9 / review L3).
///
/// Both inputs are folded into a fixed-width digest with a per-process random
/// key, and the digests are compared with a fixed 8-iteration loop. The old
/// implementation returned `false` immediately when the lengths differed,
/// letting an attacker measure how long the expected token was; the digest
/// step also keeps the loop count independent of input length. The random key
/// (rather than `DefaultHasher`'s fixed keys) prevents an offline attacker from
/// crafting a collision that would pass authentication.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hash, Hasher};

    // One random state → both digests share the same key, so equal inputs
    // produce equal digests while a different process key defeats precomputed
    // collisions.
    let state = RandomState::new();
    let mut ha = state.build_hasher();
    a.hash(&mut ha);
    let mut hb = state.build_hasher();
    b.hash(&mut hb);
    let x = ha.finish().to_le_bytes();
    let y = hb.finish().to_le_bytes();

    // Fixed 8-iteration loop: the count never depends on input length.
    let mut diff = 0u8;
    for (xa, ya) in x.iter().zip(y.iter()) {
        diff |= xa ^ ya;
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

    let bytes = axum::body::to_bytes(body, MAX_BODY_BYTES)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("read body: {e}")))?;

    // A session id only means something when THIS server issued it. Accepting any
    // string let a client subscribe to another client's replies by guessing one
    // (`"1"`, `"default"`), and a request that named none was broadcast to every
    // subscriber — the leak the session map exists to prevent (audit C4).
    let (session_id, issued) = resolve_session(&state, session_header(&headers), &bytes)?;
    let mut response = forward_message(&state, session_id, bytes).await?;
    // Echo a freshly minted id so the client can name it from now on.
    if let Some(session_id) = issued
        && let Ok(value) = axum::http::HeaderValue::from_str(&session_id)
    {
        response.headers_mut().insert(SESSION_HEADER, value);
    }
    Ok(response)
}

/// Forward one authorized, session-resolved POST body to the protocol loop and
/// collect the matching replies.
///
/// # Errors
///
/// Returns `BAD_REQUEST` for a malformed body and `INTERNAL_SERVER_ERROR` when the
/// protocol loop has already shut down.
async fn forward_message(
    state: &AppState,
    session_id: String,
    bytes: axum::body::Bytes,
) -> std::result::Result<axum::response::Response, (StatusCode, String)> {
    // Subscribe BEFORE forwarding so a fast response is not missed: the
    // protocol loop may answer before this handler awaits recv.
    let mut response_rx = state.session_channel(&session_id).subscribe();
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
                .send((Some(session_id.clone()), msg))
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
            .send((Some(session_id), msg))
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

    // Wait for responses. A single-request POST returns the matching
    // Response object; a BATCH must collect EVERY expected id and return a
    // JSON array (JSON-RPC 2.0 §6) — returning only the first match left the
    // other N−1 responses lost on the channel with no SSE subscriber.
    //
    // `is_batch` is derived from the REQUEST SHAPE (a JSON array body), not
    // from the number of matched ids: a one-element batch `[{id:1}]` must
    // still get a one-element array reply per §6.
    let is_batch = bytes.starts_with(b"[");
    let matched = tokio::time::timeout(POST_RESPONSE_TIMEOUT, async {
        let mut collected: Vec<JSONRPCMessage> = Vec::new();
        let mut remaining = expected_ids.clone();
        while !remaining.is_empty() {
            match response_rx.recv().await {
                Ok(JSONRPCMessage::Response(resp)) if remaining.contains(&resp.id) => {
                    remaining.retain(|id| id != &resp.id);
                    collected.push(JSONRPCMessage::Response(resp));
                }
                Ok(_) => continue, // another request's response; keep waiting
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
        collected
    })
    .await;

    match matched {
        Ok(collected) if !collected.is_empty() => {
            let body_value = if is_batch {
                serde_json::to_value(&collected)
            } else {
                // Single request: preserve the object (not a 1-element array).
                serde_json::to_value(collected.into_iter().next().expect("non-empty"))
            }
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("serialize: {e}")))?;
            let json = serde_json::to_string(&body_value)
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

    // A session must be presented AND must be one this server issued: a stream
    // keyed by a guessed id let a client watch another client's replies, and the
    // no-session fallback streamed a global broadcast to every subscriber — so two
    // clients that both omitted the header received each other's responses
    // (audit C4).
    let session_id = session_header(&headers).ok_or((
        StatusCode::BAD_REQUEST,
        format!("missing `{SESSION_HEADER}`; POST `initialize` to obtain one"),
    ))?;
    if !state.knows_session(&session_id) {
        return Err((
            StatusCode::NOT_FOUND,
            format!("unknown session `{session_id}`; POST `initialize` to obtain one"),
        ));
    }
    // Per-session stream: subscribe to the session's OWN channel so concurrent
    // clients never see each other's responses.
    let rx = state.session_channel(&session_id).subscribe();

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
    // Refuse to start with a blank token: it looks like authentication is on
    // while the server effectively accepts an empty credential (audit L9).
    // Validated here (not only in `CliArgs::into_config`) so a direct caller
    // cannot bypass the check.
    crate::config::validate_http_token(token.as_deref())?;

    let (request_tx, request_rx) = mpsc::channel(256);
    // Session map shared by the transport (routing replies) and the handlers
    // (issuing/subscribing per-session channels).
    let sessions: SessionMap = Arc::new(std::sync::Mutex::new(HashMap::new()));

    let state = AppState {
        request_tx,
        sessions: sessions.clone(),
        token,
    };

    // Drive the protocol loop on a background task; it terminates when the
    // request channel closes (all POST senders dropped, i.e. server shutdown).
    tokio::spawn(async move {
        let mut transport = HttpTransport::new(request_rx, sessions);
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
mod auth_tests {
    use super::*;

    /// Objective: Verify the constant-time comparison accepts identical byte
    /// sequences, including the empty sequence.
    /// Invariants: equal inputs compare equal.
    #[test]
    fn constant_time_eq_accepts_identical_bytes() {
        assert!(
            constant_time_eq(b"sekrit-token", b"sekrit-token"),
            "equal byte strings must compare equal"
        );
        assert!(
            constant_time_eq(b"", b""),
            "two empty byte strings must compare equal"
        );
    }

    /// Objective: Verify a single differing byte is rejected (the comparison
    /// does not silently accept near-matches).
    /// Invariants: one differing byte → false.
    #[test]
    fn constant_time_eq_rejects_different_bytes() {
        assert!(
            !constant_time_eq(b"sekrit-token", b"sekrit-tokee"),
            "a single differing byte must compare unequal"
        );
    }

    /// Objective: Verify unequal lengths are rejected. The old implementation
    /// returned early on a length mismatch, letting an attacker measure the
    /// expected token's length; the digest path must not (audit L9).
    /// Invariants: a length mismatch → false, without a length-based early
    /// return.
    #[test]
    fn constant_time_eq_rejects_unequal_lengths() {
        assert!(
            !constant_time_eq(b"sekrit", b"sekrit-token"),
            "a length mismatch must compare unequal"
        );
    }

    /// Objective: Verify `serve_http` refuses a blank token so a direct caller
    /// cannot start an effectively unauthenticated server even when the CLI
    /// path was bypassed (audit L9).
    /// Invariants: `serve_http(.., Some("   "))` returns `Err`.
    #[tokio::test]
    async fn serve_http_rejects_a_blank_token() {
        let server = MCPServer::new(crate::mcp::types::Implementation {
            name: "auth-test".into(),
            version: "0.0.1".into(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral port");
        let result = serve_http(server, listener, Some("   ".to_string())).await;
        assert!(
            result.is_err(),
            "a whitespace-only token must be refused before serving, got {result:?}"
        );
    }
}

#[cfg(test)]
#[path = "http_server_tests.rs"]
mod tests;
