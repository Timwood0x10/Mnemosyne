//! End-to-end MCP test: attach the real corpus/1.pdf (encrypted PDF).
//!
//! corpus/1.pdf carries an `/Encrypt` dictionary, which the built-in
//! extractor deliberately rejects (InvalidInput, not a silent garbage dump).
//! This test verifies the MCP `knowledge_attach` path surfaces that error
//! gracefully instead of crashing or materializing garbage.
//!
//! Run: cargo test --test one_pdf_mcp -- --nocapture

use std::sync::{Arc, RwLock};

use lore_scope::knowledge::{
    EntityLinker, ExternalKnowledgeRegistry, KnowledgeStore, SQLiteKnowledgeStore,
};
use lore_scope::mcp::external_knowledge_tools::register_external_knowledge_tools;
use lore_scope::mcp::types::{JSONRPCMessage, JSONRPCRequest};
use lore_scope::mcp::{ServerBuilder, Transport};

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

async fn call_tool(
    server: &lore_scope::mcp::MCPServer,
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

/// Objective: Verify attaching an encrypted PDF fails gracefully via MCP.
/// Invariants: attach returns an error result with a clear message; the
/// knowledge store stays empty (no garbage materialized).
#[tokio::test]
async fn attach_encrypted_pdf_fails_gracefully() {
    let pdf_path = std::path::Path::new("corpus/1.pdf");
    if !pdf_path.exists() {
        eprintln!("⚠  corpus/1.pdf not present — skipping encrypted-PDF MCP test");
        return;
    }

    let (server, store) = {
        // Rebuild with access to the store for the empty-graph assertion.
        let store = Arc::new(
            SQLiteKnowledgeStore::open_in_memory()
                .await
                .expect("knowledge store"),
        );
        let fact_store = Arc::new(
            lore_scope::fact_store::SqliteFactStore::open_in_memory().expect("fact store"),
        );
        let registry = Arc::new(ExternalKnowledgeRegistry::new());
        let linker: Arc<RwLock<EntityLinker>> = Arc::new(RwLock::new(EntityLinker::new()));
        let mut builder = ServerBuilder::new(lore_scope::mcp::types::Implementation {
            name: "test".into(),
            version: "1.0.0".into(),
        });
        builder =
            register_external_knowledge_tools(builder, registry, linker, store.clone(), fact_store)
                .await;
        (builder.build(), store)
    };

    let resp = call_tool(
        &server,
        "knowledge_attach",
        serde_json::json!({
            "source_type": "document",
            "path": "corpus/1.pdf",
            "source_name": "encrypted-one"
        }),
    )
    .await;
    eprintln!("attach response: {resp}");

    // The attach must report failure (isError=true) with a message that
    // explains the encryption limitation — never a silent partial success.
    assert_eq!(
        resp["isError"],
        serde_json::Value::Bool(true),
        "encrypted PDF attach must fail"
    );
    let text = resp["content"][0]["text"].as_str().expect("text");
    assert!(
        text.contains("ncrypt"),
        "error must mention encryption; got: {text}"
    );

    // No document may have been created — the graph must stay empty.
    let docs = store
        .find_document_by_title("encrypted-one")
        .await
        .expect("query");
    assert!(docs.is_none(), "no garbage document may be materialized");
}
