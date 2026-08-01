//! Integration tests for the resume portrait pipeline:
//!   1. Real corpus/2.pdf → pdf_oxide → PortraitExtractor end-to-end.
//!   2. The `portrait_extract` MCP tool over JSON-RPC.
//!
//! Run: cargo test --test portrait -- --nocapture

use lore_scope::knowledge::portrait::PortraitExtractor;
use lore_scope::mcp::portrait_tool::{PortraitTool, portrait_extract_definition};
use lore_scope::mcp::types::{JSONRPCMessage, JSONRPCRequest};
use lore_scope::mcp::{MCPServer, ServerBuilder, Transport};

/// Minimal in-memory transport for driving the MCP server in tests.
struct MemoryTransport {
    inbox: Vec<JSONRPCMessage>,
    outbox: Vec<JSONRPCMessage>,
}

#[async_trait::async_trait]
impl Transport for MemoryTransport {
    async fn recv(&mut self) -> lore_scope::error::Result<Option<JSONRPCMessage>> {
        Ok(self.inbox.pop())
    }
    async fn send(&mut self, msg: &JSONRPCMessage) -> lore_scope::error::Result<()> {
        self.outbox.push(msg.clone());
        Ok(())
    }
}

/// Build a server with only the `portrait_extract` tool registered.
async fn portrait_server() -> MCPServer {
    let mut builder = ServerBuilder::new(lore_scope::mcp::types::Implementation {
        name: "test".into(),
        version: "1.0.0".into(),
    });
    builder = builder
        .tool(
            portrait_extract_definition(),
            std::sync::Arc::new(PortraitTool::new()),
        )
        .await;
    builder.build()
}

/// Drive one `tools/call` request; return the JSON result value.
async fn call_portrait(server: &MCPServer, args: serde_json::Value) -> serde_json::Value {
    let mut t = MemoryTransport {
        inbox: vec![JSONRPCMessage::Request(JSONRPCRequest {
            jsonrpc: "2.0".into(),
            id: serde_json::Value::from(1),
            method: "tools/call".into(),
            params: Some(serde_json::json!({"name": "portrait_extract", "arguments": args})),
        })],
        outbox: vec![],
    };
    server.serve(&mut t).await.expect("serve");
    let JSONRPCMessage::Response(resp) = &t.outbox[0] else {
        panic!("expected JSON-RPC response");
    };
    resp.result.as_ref().expect("result").clone()
}

/// Objective: Verify the real 2.pdf resume flows through pdf_oxide into a
/// complete portrait end-to-end.
/// Invariants: name == 师琤琤, position contains 工程师, ≥2 projects with
/// non-empty names, contacts include the email.
#[test]
fn real_resume_pdf_portrait() {
    let pdf_path = std::path::Path::new("corpus/2.pdf");
    if !pdf_path.exists() {
        eprintln!("⚠  corpus/2.pdf not present — skipping real-PDF portrait test");
        return;
    }
    let bytes = std::fs::read(pdf_path).expect("read 2.pdf");
    let text = lore_scope::knowledge::pdf::extract_text(&bytes).expect("extract pdf text");

    let extractor = PortraitExtractor::new();
    let portrait = extractor.extract(&text).expect("portrait must extract");

    assert_eq!(portrait.name, "师琤琤", "resume name must extract exactly");
    assert!(
        portrait.position.contains("工程师"),
        "position must contain 工程师, got {:?}",
        portrait.position
    );
    assert!(
        portrait
            .contact
            .iter()
            .any(|c| c.contains("scchain1998@163.com")),
        "email must be in contacts"
    );
    assert!(
        portrait.projects.len() >= 2,
        "resume must expose at least two projects, got {}",
        portrait.projects.len()
    );
    assert!(
        portrait.projects.iter().all(|p| !p.name.is_empty()),
        "every project must have a non-empty name"
    );

    eprintln!(
        "portrait: name={} position={} projects={}",
        portrait.name,
        portrait.position,
        portrait
            .projects
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// Objective: Verify the `portrait_extract` MCP tool returns a portrait over
/// JSON-RPC for a synthetic resume.
/// Invariants: isError=false; portrait.name/position/projects match input.
#[tokio::test]
async fn mcp_portrait_tool_roundtrip() {
    let server = portrait_server().await;
    let resp = call_portrait(
        &server,
        serde_json::json!({
            "source_name": "synthetic",
            "text": "Alice Wonder\n\nSenior Backend Engineer\n\n| alice@corp.com | 12345\nGitHub:alicew\n\nSkills\n\nRust, Go, SQL\n\nProjects\n\nCore—Distributed Runtime\nProduction runtime.\n"
        }),
    )
    .await;

    assert_eq!(resp["isError"], serde_json::Value::Bool(false));
    let text = resp["content"][0]["text"].as_str().expect("text payload");
    let payload: serde_json::Value = serde_json::from_str(text).expect("payload is JSON");

    assert_eq!(payload["source_name"], "synthetic");
    assert_eq!(payload["portrait"]["name"], "Alice Wonder");
    assert!(
        payload["portrait"]["position"]
            .as_str()
            .unwrap_or("")
            .contains("Engineer"),
        "position must contain Engineer"
    );
    let projects = payload["portrait"]["projects"]
        .as_array()
        .expect("projects array");
    assert_eq!(projects.len(), 1, "one `—` project expected");
    assert_eq!(projects[0]["name"], "Core");
}

/// Objective: Verify the MCP tool surfaces a typed error for empty input.
/// Invariants: isError=true and the message mentions the empty input cause.
#[tokio::test]
async fn mcp_portrait_tool_rejects_empty_input() {
    let server = portrait_server().await;
    let resp = call_portrait(&server, serde_json::json!({"text": "   \n\n"})).await;

    assert_eq!(resp["isError"], serde_json::Value::Bool(true));
    let text = resp["content"][0]["text"].as_str().expect("text payload");
    assert!(
        text.contains("empty"),
        "error must explain the empty input, got: {text}"
    );
}

/// Objective: Verify the MCP tool rejects missing required `text`.
/// Invariants: Missing required args yield a JSON-RPC -32602 invalid-params
/// error (no `result`), surfaced without a panic.
#[tokio::test]
async fn mcp_portrait_tool_requires_text() {
    let server = portrait_server().await;
    let mut t = MemoryTransport {
        inbox: vec![JSONRPCMessage::Request(JSONRPCRequest {
            jsonrpc: "2.0".into(),
            id: serde_json::Value::from(1),
            method: "tools/call".into(),
            params: Some(serde_json::json!({"name": "portrait_extract", "arguments": {}})),
        })],
        outbox: vec![],
    };
    server.serve(&mut t).await.expect("serve");
    let JSONRPCMessage::Response(resp) = &t.outbox[0] else {
        panic!("expected JSON-RPC response");
    };
    // Schema validation returns a JSON-RPC error, not a tool result.
    assert!(
        resp.result.is_none(),
        "missing required arg must not produce a result"
    );
    let err = resp.error.as_ref().expect("error present");
    assert_eq!(err.code, -32602, "must be invalid-params error");
    assert!(
        err.message.contains("text"),
        "error must mention the missing `text` argument, got: {}",
        err.message
    );
}
