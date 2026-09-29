//! Unit tests for the MCP Streamable HTTP transport.
//!
//! Extracted from `http_server.rs` via `#[path]` so the module source stays under
//! the 1000-line limit (`plan/rules/rules.md` §1). The tests reach the parent's
//! private items through `use super::*`.
//!
//! The socket-level cases are `#[ignore]`d so the fast `make test` inner loop
//! skips them; run them with `cargo test -- --ignored`. The pure-logic cases
//! (session header parsing, idle eviction) run everywhere.

use super::*;
use crate::mcp::server::MCPServer;
use crate::mcp::types::{Implementation, JSONRPCRequest, JSONRPCResponse};
use axum::body::Body;
use axum::http::Request as HttpRequest;
use serde_json::json;
use tower::ServiceExt;

/// An empty session map, as a freshly started server has.
fn test_sessions() -> SessionMap {
    Arc::new(std::sync::Mutex::new(HashMap::new()))
}

/// Register a session as if the server had issued it, and return the id a client
/// would present.
///
/// The tests need a KNOWN id: the server only honours sessions it minted
/// (audit C4), and real ids are random by design.
fn issue_test_session(sessions: &SessionMap) -> String {
    let session_id = format!("test-session-{}", sessions.lock().expect("lock").len());
    sessions.lock().expect("lock").insert(
        session_id.clone(),
        SessionEntry {
            sender: broadcast::channel(SESSION_CHANNEL_CAPACITY).0,
            last_seen: Instant::now(),
        },
    );
    session_id
}

/// A test state plus the receiving end of the request channel.
fn test_state(
    sessions: &SessionMap,
    token: Option<String>,
) -> (AppState, mpsc::Receiver<(Option<String>, JSONRPCMessage)>) {
    let (request_tx, request_rx) = mpsc::channel::<(Option<String>, JSONRPCMessage)>(8);
    (
        AppState {
            request_tx,
            sessions: sessions.clone(),
            token,
        },
        request_rx,
    )
}

fn request_msg(id: i64, method: &str) -> JSONRPCMessage {
    JSONRPCMessage::Request(JSONRPCRequest {
        jsonrpc: "2.0".into(),
        id: json!(id),
        method: method.into(),
        params: None,
    })
}

/// The `initialize` request every session starts with.
fn initialize_msg() -> JSONRPCMessage {
    JSONRPCMessage::Request(JSONRPCRequest {
        jsonrpc: "2.0".into(),
        id: json!(1),
        method: "initialize".into(),
        params: Some(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0.0.1"}
        })),
    })
}

/// A `POST /message` request, optionally naming a session.
fn post(body: &str, session: Option<&str>) -> HttpRequest<Body> {
    let mut builder = HttpRequest::builder()
        .method("POST")
        .uri("/message")
        .header("content-type", "application/json");
    if let Some(session) = session {
        builder = builder.header(SESSION_HEADER, session);
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

/// Objective: Verify HttpTransport forwards a request to `recv` and routes the
/// reply to that session's OWN channel (never a shared one).
/// Invariants: request arrives intact; the response is observed by the session's
/// subscriber.
#[ignore]
#[tokio::test]
async fn transport_round_trips_over_channels() {
    let (request_tx, request_rx) = mpsc::channel::<(Option<String>, JSONRPCMessage)>(8);
    let sessions = test_sessions();
    let session_id = issue_test_session(&sessions);
    let mut response_rx = sessions
        .lock()
        .expect("lock")
        .get(&session_id)
        .expect("session entry")
        .sender
        .subscribe();
    let mut transport = HttpTransport::new(request_rx, sessions);

    request_tx
        .send((Some(session_id), request_msg(1, "tools/list")))
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

/// Objective: Verify HttpTransport yields `None` (EOF) when the request channel
/// is fully closed.
/// Invariants: dropping all senders → recv → Ok(None).
#[ignore]
#[tokio::test]
async fn transport_eof_on_closed_channel() {
    let (request_tx, request_rx) = mpsc::channel::<(Option<String>, JSONRPCMessage)>(8);
    let mut transport = HttpTransport::new(request_rx, test_sessions());
    drop(request_tx); // no senders → EOF
    let got = transport.recv().await.expect("recv");
    assert!(got.is_none(), "closed request channel → clean EOF");
}

/// Objective: Verify `POST /initialize` without a session ISSUES one and hands it
/// back in the response header, and that the issued id then works.
///
/// The server used to accept any string a client sent and never issued one, so
/// guessing `"1"` was enough to reach another client's replies — and a request
/// that named none was broadcast to every subscriber (audit C4).
/// Invariants: 200 with a non-empty `Mcp-Session-Id`; the id is registered; a
/// follow-up request naming it is accepted.
#[ignore]
#[tokio::test]
async fn initialize_issues_a_session_and_echoes_it() {
    let (request_tx, request_rx) = mpsc::channel::<(Option<String>, JSONRPCMessage)>(8);
    let sessions = test_sessions();
    let server = MCPServer::new(Implementation {
        name: "lorescope-test".into(),
        version: "0.0.1".into(),
    });
    let transport_sessions = sessions.clone();
    let handle = tokio::spawn(async move {
        let mut transport = HttpTransport::new(request_rx, transport_sessions);
        server.serve(&mut transport).await.expect("serve")
    });
    let state = AppState {
        request_tx: request_tx.clone(),
        sessions: sessions.clone(),
        token: None,
    };
    let app = router(state);

    let response = app
        .clone()
        .oneshot(post(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.0.1"}}}"#,
            None,
        ))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK, "initialize is served");
    let issued = response
        .headers()
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .expect("the response must carry the issued session id")
        .to_string();
    assert!(!issued.is_empty(), "the issued id must not be empty");
    assert!(
        sessions.lock().expect("lock").contains_key(&issued),
        "the issued id must be registered server-side"
    );

    let follow_up = app
        .oneshot(post(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            Some(&issued),
        ))
        .await
        .expect("oneshot");
    assert_eq!(
        follow_up.status(),
        StatusCode::OK,
        "a request naming the issued session is served"
    );

    drop(request_tx);
    let _ = handle.await;
}

/// Objective: Verify a request that names NO session is refused unless it is an
/// `initialize`. The old code accepted it and broadcast the reply to every
/// subscriber (audit C4).
/// Invariants: plain request without a session → 400.
#[ignore]
#[tokio::test]
async fn message_without_a_session_is_rejected() {
    let sessions = test_sessions();
    let (state, _request_rx) = test_state(&sessions, None);
    let app = router(state);

    let response = app
        .oneshot(post(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, None))
        .await
        .expect("oneshot");
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "a request must present a session"
    );
}

/// Objective: Verify a request naming a session this server did NOT issue is
/// refused. Any string used to be accepted (and created a channel on demand), so
/// a guessed id subscribed a client to another client's replies (audit C4).
/// Invariants: unknown session → 404 on POST and on GET /sse.
#[ignore]
#[tokio::test]
async fn unknown_session_is_rejected() {
    let sessions = test_sessions();
    let (state, _request_rx) = test_state(&sessions, None);
    let app = router(state);

    let post_response = app
        .clone()
        .oneshot(post(
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
            Some("guessed"),
        ))
        .await
        .expect("oneshot");
    assert_eq!(
        post_response.status(),
        StatusCode::NOT_FOUND,
        "an unissued session id must not be honoured"
    );

    let sse_response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/sse")
                .header(SESSION_HEADER, "guessed")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(
        sse_response.status(),
        StatusCode::NOT_FOUND,
        "an unissued session id must not open a stream"
    );
}

/// Objective: Verify `GET /sse` needs a session it issued, and that a valid one
/// streams `text/event-stream`.
/// Invariants: missing session → 400; issued session → 200 + event-stream.
#[ignore]
#[tokio::test]
async fn sse_requires_a_known_session() {
    let sessions = test_sessions();
    let (state, _request_rx) = test_state(&sessions, None);
    let app = router(state);

    let muteless = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/sse")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(
        muteless.status(),
        StatusCode::BAD_REQUEST,
        "a stream must present a session"
    );

    let session_id = issue_test_session(&sessions);
    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/sse")
                .header(SESSION_HEADER, session_id)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK, "SSE stream opens");
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("text/event-stream"),
        "SSE content-type, got {content_type:?}"
    );
}

/// Objective: Verify `POST /message` accepts a well-formed JSON-RPC message from a
/// known session and replies 202 Accepted.
/// Invariants: status 202; the forwarded message reaches the request channel
/// tagged with the session.
#[ignore]
#[tokio::test]
async fn post_message_accepts_and_returns_202() {
    let sessions = test_sessions();
    let session_id = issue_test_session(&sessions);
    let (state, mut request_rx) = test_state(&sessions, None);
    let app = router(state);

    let response = app
        .oneshot(post(
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
            Some(&session_id),
        ))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::ACCEPTED, "202 on accepted");

    let (session, msg) = request_rx.recv().await.expect("message forwarded");
    assert_eq!(
        session.as_deref(),
        Some(session_id.as_str()),
        "the forwarded request keeps its session"
    );
    match msg {
        JSONRPCMessage::Request(r) => assert_eq!(r.method, "ping", "message forwarded"),
        other => panic!("expected Request, got {other:?}"),
    }
}

/// Objective: Verify authentication is enforced when a token is set.
/// Invariants: wrong/missing bearer → 401; correct bearer with a session → 202.
#[ignore]
#[tokio::test]
async fn auth_enforced_when_token_set() {
    let sessions = test_sessions();
    let session_id = issue_test_session(&sessions);
    let (state, _request_rx) = test_state(&sessions, Some("secret".into()));
    let app = router(state);

    let no_auth = app
        .clone()
        .oneshot(post(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, None))
        .await
        .expect("oneshot");
    assert_eq!(
        no_auth.status(),
        StatusCode::UNAUTHORIZED,
        "missing token → 401"
    );

    let wrong = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/message")
                .header("authorization", "Bearer wrong")
                .header(SESSION_HEADER, &session_id)
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

    let ok = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/message")
                .header("authorization", "Bearer secret")
                .header(SESSION_HEADER, session_id)
                .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#))
                .expect("request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(ok.status(), StatusCode::ACCEPTED, "correct token → 202");
}

/// Objective: Verify a malformed JSON-RPC body on POST is rejected with 400, not
/// accepted — the body is parsed after the session is resolved, so this also
/// pins that the session gate does not swallow the JSON error for a valid client.
/// Invariants: invalid JSON with a known session → 400 Bad Request.
#[ignore]
#[tokio::test]
async fn malformed_body_rejected_400() {
    let sessions = test_sessions();
    let session_id = issue_test_session(&sessions);
    let (state, _request_rx) = test_state(&sessions, None);
    let app = router(state);

    let response = app
        .oneshot(post("not json", Some(&session_id)))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST, "bad JSON → 400");
}

/// Objective: End-to-end — verify the real [`MCPServer::serve`] loop, driven
/// through [`HttpTransport`], answers a JSON-RPC `initialize` request on the
/// session's own channel.
/// Invariants: a tagged request yields a response carrying the server
/// `implementation`.
#[ignore]
#[tokio::test]
async fn server_answers_initialize_over_the_transport() {
    let (request_tx, request_rx) = mpsc::channel::<(Option<String>, JSONRPCMessage)>(8);
    let sessions = test_sessions();
    let session_id = issue_test_session(&sessions);
    let mut response_rx = sessions
        .lock()
        .expect("lock")
        .get(&session_id)
        .expect("session entry")
        .sender
        .subscribe();
    let server = MCPServer::new(Implementation {
        name: "lorescope-test".into(),
        version: "0.0.1".into(),
    });

    let handle = tokio::spawn(async move {
        let mut transport = HttpTransport::new(request_rx, sessions);
        server.serve(&mut transport).await.expect("serve")
    });

    request_tx
        .send((Some(session_id), initialize_msg()))
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
            assert_eq!(
                result["serverInfo"]["name"].as_str().unwrap_or(""),
                "lorescope-test",
                "server identifies itself"
            );
        }
        other => panic!("expected Response, got {other:?}"),
    }

    // Shut down cleanly and await the serve task.
    drop(request_tx);
    let _ = handle.await;
}

/// Objective: Verify a `POST /initialize` with NO `/sse` subscriber still receives
/// its JSON-RPC response in the POST body — the spec's "respond in POST body when
/// no stream is open" fallback — and that the SAME session map is shared by the
/// transport and the handlers (they used to be two different maps, which the old
/// global broadcast papered over).
/// Invariants: HTTP 200; body is the JSON-RPC response echoing the id.
#[ignore]
#[tokio::test]
async fn post_message_returns_response_body_without_sse() {
    let (request_tx, request_rx) = mpsc::channel::<(Option<String>, JSONRPCMessage)>(8);
    let sessions = test_sessions();
    let server = MCPServer::new(Implementation {
        name: "lorescope-test".into(),
        version: "0.0.1".into(),
    });
    let transport_sessions = sessions.clone();
    let handle = tokio::spawn(async move {
        let mut transport = HttpTransport::new(request_rx, transport_sessions);
        server.serve(&mut transport).await.expect("serve")
    });

    // No SSE subscriber is ever created — the response must come back in the POST
    // body (the regression this test locks in). `request_tx` is kept outside so
    // the test can drop it at the end to signal EOF to the serve loop.
    let state = AppState {
        request_tx: request_tx.clone(),
        sessions,
        token: None,
    };
    let app = router(state);

    let response = app
        .oneshot(post(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.0.1"}}}"#,
            None,
        ))
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
            .is_some_and(|name| name == "lorescope-test"),
        "initialize result present in POST body, got {resp}"
    );

    // Shut down cleanly and await the serve task.
    drop(request_tx);
    let _ = handle.await;
}

/// Objective: Verify the session header is read under both the spec spelling and
/// the legacy lowercase one, so switching to the spec name did not break clients.
/// Invariants: `Mcp-Session-Id` wins when present, `x-mcp-session-id` still works,
/// blank/missing values read as absent.
#[test]
fn session_header_reads_both_spellings() {
    let with_spec = HeaderMap::from_iter([(
        axum::http::HeaderName::from_static(SESSION_HEADER),
        axum::http::HeaderValue::from_static(" spec-id "),
    )]);
    assert_eq!(
        session_header(&with_spec).as_deref(),
        Some("spec-id"),
        "the spec header is trimmed and honoured"
    );

    let with_legacy = HeaderMap::from_iter([(
        axum::http::HeaderName::from_static(LEGACY_SESSION_HEADER),
        axum::http::HeaderValue::from_static("legacy-id"),
    )]);
    assert_eq!(
        session_header(&with_legacy).as_deref(),
        Some("legacy-id"),
        "the legacy header still works"
    );

    let blank = HeaderMap::from_iter([(
        axum::http::HeaderName::from_static(SESSION_HEADER),
        axum::http::HeaderValue::from_static("   "),
    )]);
    assert_eq!(session_header(&blank), None, "a blank id is an absent id");
    assert_eq!(session_header(&HeaderMap::new()), None, "no header → None");
}

/// Objective: Verify idle sessions are reclaimed. Nothing removed an entry when a
/// client went away, so every disconnect left a 1024-slot broadcast channel behind
/// for the life of the process (audit H8).
/// Invariants: entries older than the TTL are dropped; a fresh one survives.
#[test]
fn stale_sessions_are_evicted() {
    let now = Instant::now();
    let stale = now
        .checked_sub(SESSION_TTL + Duration::from_secs(1))
        .expect("a representable instant in the past");
    let mut sessions = HashMap::new();
    sessions.insert(
        "stale".to_string(),
        SessionEntry {
            sender: broadcast::channel(4).0,
            last_seen: stale,
        },
    );
    sessions.insert(
        "fresh".to_string(),
        SessionEntry {
            sender: broadcast::channel(4).0,
            last_seen: now,
        },
    );

    assert_eq!(
        evict_stale_sessions(&mut sessions, now),
        1,
        "exactly the idle session is dropped"
    );
    assert!(
        sessions.contains_key("fresh") && !sessions.contains_key("stale"),
        "the fresh session survives, got {:?}",
        sessions.keys().collect::<Vec<_>>()
    );
}
