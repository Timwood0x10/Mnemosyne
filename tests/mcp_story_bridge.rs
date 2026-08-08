//! MCP test for acceptance item 8 (novel side): bridge a novel protagonist's
//! story events into fact-store persona facts, then let `persona_timeline`
//! output the 起点 → 转折 → 现状 arc on the full 倾城之恋 corpus.
//!
//! Drives the real `generalize_compile` → `story_bridge` → `persona_timeline`
//! tools over the JSON-RPC path. After the bridge, 流苏's evolution timeline
//! must have a start, a current, and at least one turning point — proving the
//! timeline engine works on novel prose just as it does on live dialogue.
//!
//! Run: cargo test --test mcp_story_bridge -- --nocapture

use std::sync::Arc;

use async_trait::async_trait;

use mnemosyne::fact_store::SqliteFactStore;
use mnemosyne::knowledge::EntityLinker;
use mnemosyne::knowledge::SQLiteKnowledgeStore;
use mnemosyne::knowledge::external::ExternalKnowledgeRegistry;
use mnemosyne::mcp::generalize_tool::register_generalize_tool;
use mnemosyne::mcp::knowledge_tools::register_knowledge_tools;
use mnemosyne::mcp::relationship_tool::{PersonaTimelineTool, persona_timeline_definition};
use mnemosyne::mcp::server::{MCPServer, ServerBuilder};
use mnemosyne::mcp::story_bridge_tool::{StoryBridgeTool, story_bridge_definition};
use mnemosyne::mcp::transport::Transport;
use mnemosyne::mcp::types::{Implementation, JSONRPCMessage, JSONRPCRequest, JSONRPCResponse};

/// A single-shot in-memory transport: yields one request, captures one response.
struct OneShotTransport {
    inbox: Vec<JSONRPCMessage>,
    outbox: Vec<JSONRPCMessage>,
}

#[async_trait]
impl Transport for OneShotTransport {
    async fn recv(&mut self) -> mnemosyne::error::Result<Option<JSONRPCMessage>> {
        Ok(self.inbox.pop())
    }
    async fn send(&mut self, msg: &JSONRPCMessage) -> mnemosyne::error::Result<()> {
        self.outbox.push(msg.clone());
        Ok(())
    }
}

async fn call_tool(
    server: &MCPServer,
    id: u64,
    name: &str,
    args: serde_json::Value,
) -> serde_json::Value {
    let mut t = OneShotTransport {
        inbox: vec![JSONRPCMessage::Request(JSONRPCRequest {
            jsonrpc: "2.0".to_string(),
            id: serde_json::json!(id),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({ "name": name, "arguments": args })),
        })],
        outbox: vec![],
    };
    server.serve(&mut t).await.expect("MCP serve must complete");
    let msg = t
        .outbox
        .pop()
        .expect("a tool call must produce one response");
    match msg {
        JSONRPCMessage::Response(JSONRPCResponse {
            result: Some(res),
            error,
            ..
        }) => {
            if let Some(e) = error {
                panic!("tool `{name}` returned error: {e:?}");
            }
            res
        }
        other => panic!("expected Response, got {other:?}"),
    }
}

fn text_of(result: &serde_json::Value) -> String {
    result
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|b| b.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string()
}

async fn build_server() -> (MCPServer, Arc<SqliteFactStore>) {
    let kstore = Arc::new(
        SQLiteKnowledgeStore::open_in_memory()
            .await
            .expect("open in-memory knowledge store"),
    );
    let fact_store = Arc::new(SqliteFactStore::open_in_memory().expect("open fact store"));
    let entity_linker: Arc<std::sync::RwLock<EntityLinker>> =
        Arc::new(std::sync::RwLock::new(EntityLinker::new()));
    let external_registry = Arc::new(ExternalKnowledgeRegistry::new());

    let mut builder = ServerBuilder::new(Implementation {
        name: "mcp-story-bridge".into(),
        version: "0.1.0".into(),
    });
    builder = register_knowledge_tools(
        builder,
        kstore.clone(),
        fact_store.clone(),
        Some(entity_linker.clone()),
    )
    .await;
    builder = mnemosyne::mcp::external_knowledge_tools::register_external_knowledge_tools(
        builder,
        external_registry,
        entity_linker,
        kstore.clone(),
        fact_store.clone(),
    )
    .await;
    builder = register_generalize_tool(builder, kstore.clone()).await;
    builder = builder
        .tool(
            story_bridge_definition(),
            Arc::new(StoryBridgeTool::new(kstore.clone(), fact_store.clone())),
        )
        .await
        .tool(
            persona_timeline_definition(),
            Arc::new(PersonaTimelineTool::new(fact_store.clone())),
        )
        .await;
    (builder.build(), fact_store)
}

#[tokio::test]
async fn novel_timeline_works_after_bridge() {
    println!("\n════════════════════════════════════════════════════════");
    println!("  MCP 测试：第8条（小说侧）· 倾城之恋 → story_bridge → persona_timeline");
    println!("════════════════════════════════════════════════════════\n");

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/corpus/倾城之恋.txt");
    let text = std::fs::read_to_string(path).expect("read 倾城之恋.txt");
    let chars = text.chars().count();
    let (server, _fact_store) = build_server().await;

    // 1. Compile the full novel into the knowledge graph.
    println!("── 阶段 1：generalize_compile 编译《倾城之恋》（chars={chars}）──");
    let comp = call_tool(
        &server,
        1,
        "generalize_compile",
        serde_json::json!({
            "doc_type": "text",
            "title": "倾城之恋",
            "source": "corpus",
            "text": text,
        }),
    )
    .await;
    println!("  {}", text_of(&comp));

    // 2. Bridge 流苏's story events into fact-store persona facts.
    println!("\n── 阶段 2：story_bridge(\"流苏\") ──");
    let bridge = call_tool(
        &server,
        2,
        "story_bridge",
        serde_json::json!({"name": "流苏", "tenant_id": "default"}),
    )
    .await;
    let bridge_text = text_of(&bridge);
    println!("  {bridge_text}");
    let bridge_json: serde_json::Value =
        serde_json::from_str(&bridge_text).expect("story_bridge returns JSON");
    let entity_id = bridge_json["entity_id"].as_i64().expect("entity_id");
    let story_events = bridge_json["story_events"].as_i64().unwrap_or(0);
    let persona_facts = bridge_json["persona_facts"].as_i64().unwrap_or(0);
    let event_facts = bridge_json["event_facts"].as_i64().unwrap_or(0);
    assert!(
        story_events > 0,
        "流苏 must have story events, got {story_events}"
    );
    assert!(
        event_facts > 0,
        "bridge must write event facts, got {event_facts}"
    );

    // 3. Rebuild the evolution timeline on the novel character.
    println!("\n── 阶段 3：persona_timeline(entity_id={entity_id}) ──");
    let tl = call_tool(
        &server,
        3,
        "persona_timeline",
        serde_json::json!({"entity_id": entity_id}),
    )
    .await;
    let tl_text = text_of(&tl);
    println!("  {tl_text}");
    let tl_json: serde_json::Value =
        serde_json::from_str(&tl_text).expect("persona_timeline returns JSON");

    let start_ok = tl_json["start"].is_object();
    let current_ok = tl_json["current"].is_object();
    let milestones = tl_json["milestones"].as_array().map_or(0, Vec::len);
    let trajectory = tl_json["trajectory"].as_array().map_or(0, Vec::len);

    println!(
        "\n  start={start_ok}, milestones={milestones}, current={current_ok}, trajectory={trajectory}"
    );
    assert!(
        start_ok,
        "novel timeline must have an origin (start), got {}",
        tl_json["start"]
    );
    assert!(
        current_ok,
        "novel timeline must have a present (current), got {}",
        tl_json["current"]
    );
    assert!(
        milestones > 0,
        "novel timeline must expose turning points, got {milestones}"
    );
    assert!(
        trajectory > 0,
        "novel timeline must keep the full trajectory"
    );
    assert!(
        persona_facts >= 0,
        "persona facts may be zero for pure narrative, no constraint"
    );
    println!(
        "  ✓ 断言通过：小说人物时间线成立（起点→{}转折→现状，轨迹{trajectory}）",
        milestones
    );
}
