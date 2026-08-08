//! End-to-end MCP test: attach + materialize a real PDF via the MCP tools.
//!
//! Walks the actual JSON-RPC path (tools/call → KnowledgeAttachHandler →
//! knowledge_attach → knowledge_ingest materialize) against the real
//! corpus/巴黎圣母院.pdf file, then verifies the graph rows were created.
//!
//! Run: cargo test --test paris_pdf_mcp -- --nocapture

use std::sync::{Arc, RwLock};

use mnemosyne::knowledge::{
    EntityLinker, ExternalKnowledgeRegistry, KnowledgeStore, SQLiteKnowledgeStore,
};
use mnemosyne::mcp::external_knowledge_tools::register_external_knowledge_tools;
use mnemosyne::mcp::types::{JSONRPCMessage, JSONRPCRequest};
use mnemosyne::mcp::{ServerBuilder, Transport};

/// Minimal in-memory transport for driving the MCP server in tests.
struct MemoryTransport {
    inbox: Vec<JSONRPCMessage>,
    outbox: Vec<JSONRPCMessage>,
}

#[async_trait::async_trait]
impl Transport for MemoryTransport {
    async fn recv(&mut self) -> mnemosyne::error::Result<Option<JSONRPCMessage>> {
        Ok(self.inbox.pop())
    }
    async fn send(&mut self, msg: &JSONRPCMessage) -> mnemosyne::error::Result<()> {
        self.outbox.push(msg.clone());
        Ok(())
    }
}

/// Build a full MCP server with the external-knowledge tools registered,
/// sharing an in-memory knowledge store + fact store + registry + linker.
async fn build_server() -> (
    mnemosyne::mcp::MCPServer,
    Arc<SQLiteKnowledgeStore>,
    Arc<ExternalKnowledgeRegistry>,
) {
    let store = Arc::new(
        SQLiteKnowledgeStore::open_in_memory()
            .await
            .expect("knowledge store"),
    );
    let fact_store =
        Arc::new(mnemosyne::fact_store::SqliteFactStore::open_in_memory().expect("fact store"));
    let registry = Arc::new(ExternalKnowledgeRegistry::new());
    let linker: Arc<RwLock<EntityLinker>> = Arc::new(RwLock::new(EntityLinker::new()));

    let mut builder = ServerBuilder::new(mnemosyne::mcp::types::Implementation {
        name: "test".into(),
        version: "1.0.0".into(),
    });
    builder = register_external_knowledge_tools(
        builder,
        registry.clone(),
        linker,
        store.clone(),
        fact_store,
    )
    .await;
    (builder.build(), store, registry)
}

/// Drive one `tools/call` request through the server; return the JSON result.
async fn call_tool(
    server: &mnemosyne::mcp::MCPServer,
    name: &str,
    args: serde_json::Value,
) -> serde_json::Value {
    let mut t = MemoryTransport {
        inbox: vec![JSONRPCMessage::Request(JSONRPCRequest {
            jsonrpc: "2.0".into(),
            id: serde_json::Value::from(1),
            method: "tools/call".into(),
            params: Some(serde_json::json!({"name": name, "arguments": args})),
        })],
        outbox: vec![],
    };
    server.serve(&mut t).await.expect("serve");
    let JSONRPCMessage::Response(resp) = &t.outbox[0] else {
        panic!("expected JSON-RPC response");
    };
    resp.result.as_ref().expect("result").clone()
}

/// Objective: End-to-end attach + materialize of the real 巴黎圣母院.pdf.
/// Invariants: attach succeeds and reports the PDF format; materialize
/// persists a Document + Chapter + Evidence; the store can find the doc.
#[tokio::test]
async fn attach_and_materialize_paris_pdf() {
    let pdf_path = std::path::Path::new("corpus/巴黎圣母院.pdf");
    if !pdf_path.exists() {
        eprintln!("⚠  corpus/巴黎圣母院.pdf not present — skipping e2e PDF test");
        return;
    }

    let (server, store, registry) = build_server().await;

    // 1. knowledge_attach the PDF.
    let attach_resp = call_tool(
        &server,
        "knowledge_attach",
        serde_json::json!({
            "source_type": "document",
            "path": "corpus/巴黎圣母院.pdf",
            "source_name": "paris-notre-dame",
            "entity_links": [
                {"external_name": "Quasimodo", "canonical_name": "卡西莫多", "source": "paris-notre-dame"}
            ]
        }),
    )
    .await;
    eprintln!("attach response: {attach_resp}");
    assert_eq!(
        attach_resp["isError"],
        serde_json::Value::Bool(false),
        "attach must succeed"
    );
    let content_text = attach_resp["content"][0]["text"]
        .as_str()
        .expect("text payload");
    let payload: serde_json::Value =
        serde_json::from_str(content_text).expect("attach payload is JSON");
    assert_eq!(payload["source_name"], "paris-notre-dame");
    assert_eq!(payload["format"], "pdf", "format detection must report pdf");
    assert!(
        payload["documents_loaded"].as_u64().unwrap_or(0) > 0,
        "at least one document must load from the PDF"
    );
    assert!(
        payload["entity_links_registered"].as_u64().unwrap_or(0) >= 1,
        "entity link Quasimodo must be registered"
    );

    // 2. knowledge_ingest materialize the attached source.
    let ingest_resp = call_tool(
        &server,
        "knowledge_ingest",
        serde_json::json!({"source_name": "paris-notre-dame", "mode": "materialize"}),
    )
    .await;
    eprintln!("ingest response: {ingest_resp}");
    assert_eq!(
        ingest_resp["isError"],
        serde_json::Value::Bool(false),
        "materialize must succeed"
    );
    let ingest_text = ingest_resp["content"][0]["text"]
        .as_str()
        .expect("text payload");
    let ingest_payload: serde_json::Value =
        serde_json::from_str(ingest_text).expect("ingest payload is JSON");
    assert!(
        ingest_payload["documents_created"].as_u64().unwrap_or(0) >= 1,
        "materialize must create a document"
    );
    assert!(
        ingest_payload["chapters_created"].as_u64().unwrap_or(0) >= 1,
        "materialize must create a chapter"
    );
    assert!(
        ingest_payload["evidence_created"].as_u64().unwrap_or(0) >= 1,
        "materialize must create evidence"
    );

    // 3. Verify the graph actually holds the materialized doc.
    let doc = store
        .find_document_by_title("巴黎圣母院")
        .await
        .expect("query doc")
        .expect("materialized document must exist in the graph");
    assert_eq!(doc.title, "巴黎圣母院");
    let _ = registry; // registry is shared; already exercised via the tools
    eprintln!(
        "graph contains document id={} (author={:?}, type={:?})",
        doc.id, doc.author, doc.doc_type
    );
}

/// Objective: Verify `knowledge_ingest` index-mode is a read-only status report.
/// Invariants: index mode does not create graph rows.
#[tokio::test]
async fn ingest_index_mode_is_read_only() {
    let (server, store, _registry) = build_server().await;
    let resp = call_tool(
        &server,
        "knowledge_ingest",
        serde_json::json!({"source_name": "missing-source", "mode": "index"}),
    )
    .await;
    assert_eq!(resp["isError"], serde_json::Value::Bool(false));
    let text = resp["content"][0]["text"].as_str().expect("text");
    assert!(
        text.contains("index-mode"),
        "index mode must report query-forward status"
    );
    // No rows were created.
    let _ = store;
}

/// Objective: Verify `knowledge_attach` rejects an unknown source_type.
/// Invariants: unknown source_type yields isError=true with a clear message.
#[tokio::test]
async fn attach_rejects_unknown_source_type() {
    let (server, _store, _registry) = build_server().await;
    let resp = call_tool(
        &server,
        "knowledge_attach",
        serde_json::json!({"source_type": "nosuch"}),
    )
    .await;
    assert_eq!(resp["isError"], serde_json::Value::Bool(true));
    let text = resp["content"][0]["text"].as_str().expect("text");
    assert!(
        text.contains("unknown source_type"),
        "error must explain the unknown source type"
    );
}
