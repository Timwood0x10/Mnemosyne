//! MCP test on a real companion scenario: `corpus/bailiusu_escape.json`.
//!
//! 白流苏 (agent_id="bailiusu") is helped to escape the 白家 household and
//! start over in Hong Kong. The dialogue is rich in self-referential
//! personality signals ("我疑心重", "我不怕冷", "我宁可死在船上", "我心里……
//! 发慌"). This test drives the real `agent_fact_compile` MCP tool over the
//! JSON-RPC path and then reads the Agent entity's accumulated persona from
//! the fact store, verifying the companion-personality channel really "chats
//! out" 白流苏's persona.
//!
//! Run: cargo test --test mcp_bailiusu -- --nocapture

use std::sync::Arc;

use async_trait::async_trait;

use mnemosyne::cognition::{FactStore as _, StateEngine, build_snapshot};
use mnemosyne::fact_store::SqliteFactStore;
use mnemosyne::knowledge::EntityLinker;
use mnemosyne::knowledge::SQLiteKnowledgeStore;
use mnemosyne::knowledge::external::ExternalKnowledgeRegistry;
use mnemosyne::mcp::external_knowledge_tools::register_external_knowledge_tools;
use mnemosyne::mcp::generalize_tool::register_generalize_tool;
use mnemosyne::mcp::key_events_tool::{KeyEventsTool, key_events_definition};
use mnemosyne::mcp::knowledge_tools::register_knowledge_tools;
use mnemosyne::mcp::server::{MCPServer, ServerBuilder};
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

/// Call an MCP tool on a live server and return the JSON `result`.
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

/// Extract the first text block's string from a `tools/call` result.
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
        name: "mcp-bailiusu".into(),
        version: "0.1.0".into(),
    });
    builder = register_knowledge_tools(
        builder,
        kstore.clone(),
        fact_store.clone(),
        Some(entity_linker.clone()),
    )
    .await;
    builder = register_external_knowledge_tools(
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
            key_events_definition(),
            Arc::new(KeyEventsTool::new(kstore.clone())),
        )
        .await;
    (builder.build(), fact_store)
}

#[tokio::test]
async fn mcp_bailiusu_escape_chats_out_persona() {
    println!("\n════════════════════════════════════════════════════════");
    println!("  MCP 测试：corpus/bailiusu_escape.json（白流苏逃出白家）");
    println!("════════════════════════════════════════════════════════\n");

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/corpus/bailiusu_escape.json");
    let raw = std::fs::read_to_string(path).expect("read bailiusu_escape.json");
    let data: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    let messages = data["messages"].as_array().expect("messages array");
    let total = messages.len();
    println!("  对话加载成功：{} 条消息（{} 句）", total, total / 2);

    let (server, fact_store) = build_server().await;
    let user_id = "helper";
    let agent_id = "bailiusu";

    // ── Stage 1: feed the whole dialogue through agent_fact_compile ─────
    println!("\n── 阶段 1：agent_fact_compile 处理对话 ──");
    let chunk_json: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "role": m["role"],
                "content": m["content"],
            })
        })
        .collect();
    let res = call_tool(
        &server,
        1,
        "agent_fact_compile",
        serde_json::json!({
            "messages": chunk_json,
            "user_id": user_id,
            "agent_id": agent_id,
            "include_agent_facts": true,
        }),
    )
    .await;
    let out = text_of(&res);
    println!("  {out}");

    // ── Stage 2: read the Agent (白流苏) entity's accumulated persona ────
    println!("\n── 阶段 2：白流苏（agent）实体画像 ──");
    let agent_entity = fact_store
        .resolve_agent("default", agent_id)
        .expect("resolve agent");
    let agent_facts = fact_store.get_facts(agent_entity).expect("agent facts");
    let persona_facts: Vec<&mnemosyne::cognition::Fact> = agent_facts
        .iter()
        .filter(|f| {
            f.payload.get("attribution")
                == Some(&serde_json::Value::String("agent_personality".into()))
        })
        .collect();

    println!(
        "  Agent 实体(id={agent_entity}) 累计事实：{} 条",
        agent_facts.len()
    );
    println!(
        "  其中人格事实（attribution=agent_personality）：{} 条",
        persona_facts.len()
    );
    println!();

    // Summarize personality facets by type.
    let state_engine = StateEngine::new();
    let snapshot = build_snapshot(
        agent_entity,
        "Agent:bailiusu".to_string(),
        "Agent".to_string(),
        agent_facts.clone(),
        &state_engine,
    );
    let _ = snapshot;
    for f in persona_facts.iter().take(12) {
        let content: String = f
            .payload
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .chars()
            .take(48)
            .collect();
        println!(
            "   · [{:?}{}] {}",
            f.fact_type,
            if f.payload["negated"].as_bool().unwrap_or(false) {
                "(neg)"
            } else {
                ""
            },
            content
        );
    }

    // ── Stage 3: the User (helper) side also accumulates facts ──────────
    println!("\n── 阶段 3：用户（helper）实体画像 ──");
    let user_entity = fact_store
        .resolve_user("default", user_id)
        .expect("resolve user");
    let user_facts = fact_store.get_facts(user_entity).expect("user facts");
    println!(
        "  User 实体(id={user_entity}) 累计事实：{} 条",
        user_facts.len()
    );

    // ── Core claim ──
    assert!(
        !persona_facts.is_empty(),
        "白流苏's persona must chat out via agent_personality facts, got {}",
        persona_facts.len()
    );
    // 白流苏 should reveal personality across more than one facet.
    let mut facets = std::collections::BTreeSet::new();
    for f in &persona_facts {
        facets.insert(format!("{:?}", f.fact_type));
    }
    assert!(
        facets.len() >= 2,
        "白流苏's persona should span multiple facets, got {facets:?}"
    );
    println!(
        "\n  ✓ 断言通过：白流苏人格被聊出来了（{} 条，跨 {} 类）",
        persona_facts.len(),
        facets.len()
    );
    println!();
}
