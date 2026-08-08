//! End-to-end MCP test: attach the real corpus/1.pdf (encrypted PDF).
//!
//! corpus/1.pdf carries an `/Encrypt` dictionary, which the built-in
//! extractor deliberately rejects (InvalidInput, not a silent garbage dump).
//! This test verifies the MCP `knowledge_attach` path surfaces that error
//! gracefully instead of crashing or materializing garbage.
//!
//! Run: cargo test --test one_pdf_mcp -- --nocapture

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

/// Build a minimal single-page PDF whose trailer carries an `/Encrypt`
/// dictionary (object 6). pdf_oxide parses the file but rejects every page's
/// text extraction ("PDF is encrypted and requires a password"), which the
/// loader must surface as InvalidInput — never a silent partial success.
///
/// The test does NOT depend on `corpus/1.pdf` being encrypted (that file's
/// contents have drifted over time); it constructs the encrypted fixture
/// deterministically instead.
fn build_encrypted_pdf(content: &str) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(content.as_bytes()).expect("write");
    let compressed = encoder.finish().expect("finish");

    let obj1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec();
    let obj2 = b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec();
    let obj3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>\nendobj\n".to_vec();
    let mut obj4 = format!(
        "4 0 obj\n<< /Length {} /Filter /FlateDecode >>\nstream\n",
        compressed.len()
    )
    .into_bytes();
    obj4.extend_from_slice(&compressed);
    obj4.extend_from_slice(b"\nendstream\nendobj\n");
    let obj5 =
        b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_vec();
    // Object 6: Standard security handler — marks the document encrypted.
    let obj6 = b"6 0 obj\n<< /Filter /Standard /V 2 /R 3 /O <00000000000000000000000000000000> /U <00000000000000000000000000000000> /P -4 >>\nendobj\n".to_vec();

    let header = b"%PDF-1.4\n".to_vec();
    let o1 = header.len();
    let o2 = o1 + obj1.len();
    let o3 = o2 + obj2.len();
    let mut pdf = header;
    pdf.extend_from_slice(&obj1);
    pdf.extend_from_slice(&obj2);
    pdf.extend_from_slice(&obj3);
    let o4 = pdf.len();
    pdf.extend_from_slice(&obj4);
    let o5 = pdf.len();
    pdf.extend_from_slice(&obj5);
    let o6 = pdf.len();
    pdf.extend_from_slice(&obj6);
    let xref_offset = pdf.len();

    let mut xref = String::new();
    xref.push_str("xref\n0 7\n0000000000 65535 f \n");
    for off in [o1, o2, o3, o4, o5, o6] {
        xref.push_str(&format!("{off:010} 00000 n \n"));
    }
    xref.push_str(&format!(
        "trailer << /Size 7 /Root 1 0 R /Encrypt 6 0 R >>\nstartxref\n{xref_offset}\n%%EOF"
    ));
    pdf.extend_from_slice(xref.as_bytes());
    pdf
}

/// Objective: Verify attaching an encrypted PDF fails gracefully via MCP.
/// Invariants: attach returns an error result with a clear message; the
/// knowledge store stays empty (no garbage materialized).
#[tokio::test]
async fn attach_encrypted_pdf_fails_gracefully() {
    // Deterministic encrypted-PDF fixture in a temp file (no dependence on
    // corpus/1.pdf, whose contents have drifted).
    let bytes = build_encrypted_pdf("BT (Hello World) Tj ET");
    let tmp = std::env::temp_dir().join("lorescope_encrypted_1.pdf");
    std::fs::write(&tmp, &bytes).expect("write temp encrypted pdf");
    let pdf_path_str = tmp.to_string_lossy().to_string();

    let (server, store) = {
        // Rebuild with access to the store for the empty-graph assertion.
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
            "path": pdf_path_str,
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
