//! MCP test on a companion scenario from `corpus/WarandPeace.txt`.
//!
//! 皮埃尔·别祖霍夫 (agent_id="pierre") is talked through his identity crisis,
//! his miserable marriage to 海伦, his unspoken love for 娜塔莎, and his
//! decision to enlist. The 102-message dialogue (51 turns) is rich in
//! self-referential personality signals ("我是……的人", "我心里……", "我宁可……",
//! "我不肯……", "我想要……"). This test drives the real `agent_fact_compile`
//! MCP tool over the JSON-RPC path, then reads 皮埃尔's accumulated persona
//! from the fact store to verify the companion-personality channel chats it out.
//!
//! Run: cargo test --test mcp_warpeace -- --nocapture

use std::sync::Arc;

use async_trait::async_trait;

use lore_scope::cognition::{FactStore as _, StateEngine, build_snapshot};
use lore_scope::fact_store::SqliteFactStore;
use lore_scope::knowledge::EntityLinker;
use lore_scope::knowledge::SQLiteKnowledgeStore;
use lore_scope::knowledge::external::ExternalKnowledgeRegistry;
use lore_scope::mcp::external_knowledge_tools::register_external_knowledge_tools;
use lore_scope::mcp::generalize_tool::register_generalize_tool;
use lore_scope::mcp::key_events_tool::{KeyEventsTool, key_events_definition};
use lore_scope::mcp::knowledge_tools::register_knowledge_tools;
use lore_scope::mcp::server::{MCPServer, ServerBuilder};
use lore_scope::mcp::transport::Transport;
use lore_scope::mcp::types::{Implementation, JSONRPCMessage, JSONRPCRequest, JSONRPCResponse};

/// A single-shot in-memory transport: yields one request, captures one response.
struct OneShotTransport {
    inbox: Vec<JSONRPCMessage>,
    outbox: Vec<JSONRPCMessage>,
}

#[async_trait]
impl Transport for OneShotTransport {
    async fn recv(&mut self) -> lore_scope::error::Result<Option<JSONRPCMessage>> {
        Ok(self.inbox.pop())
    }
    async fn send(&mut self, msg: &JSONRPCMessage) -> lore_scope::error::Result<()> {
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
        name: "mcp-warpeace".into(),
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
async fn mcp_warpeace_pierre_chats_out_persona() {
    println!("\n════════════════════════════════════════════════════════");
    println!("  MCP 测试：corpus/warpeace_pierre.json（战争与和平 · 皮埃尔）");
    println!("════════════════════════════════════════════════════════\n");

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/corpus/warpeace_pierre.json");
    let raw = std::fs::read_to_string(path).expect("read warpeace_pierre.json");
    let data: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    let messages = data["messages"].as_array().expect("messages array");
    let total = messages.len();
    assert!(
        total >= 100,
        "user asked for a 100+ message dialogue, got {total}"
    );
    println!("  对话加载成功：{} 条消息（{} 句）", total, total / 2);

    let (server, fact_store) = build_server().await;
    let user_id = "listener";
    let agent_id = "pierre";

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

    // ── Stage 2: read 皮埃尔's accumulated persona ─────────────────────
    println!("\n── 阶段 2：皮埃尔（agent）实体画像 ──");
    let agent_entity = fact_store
        .resolve_agent("default", agent_id)
        .expect("resolve agent");
    let agent_facts = fact_store.get_facts(agent_entity).expect("agent facts");
    let persona_facts: Vec<&lore_scope::cognition::Fact> = agent_facts
        .iter()
        .filter(|f| {
            f.payload.get("attribution")
                == Some(&serde_json::Value::String("agent_personality".into()))
        })
        .collect();

    println!(
        "  Agent 实体(id={agent_entity}) 累计事实：{} 条，其中人格事实 {} 条",
        agent_facts.len(),
        persona_facts.len()
    );

    let state_engine = StateEngine::new();
    let _snapshot = build_snapshot(
        agent_entity,
        "Agent:pierre".to_string(),
        "Agent".to_string(),
        agent_facts.clone(),
        &state_engine,
    );

    // Show a representative sample of 皮埃尔's persona facts, grouped by facet.
    use std::collections::BTreeMap;
    let mut by_facet: BTreeMap<String, Vec<&lore_scope::cognition::Fact>> = BTreeMap::new();
    for f in &persona_facts {
        by_facet
            .entry(format!("{:?}", f.fact_type))
            .or_default()
            .push(f);
    }
    for (facet, facts) in &by_facet {
        println!("    [{facet}] x{}", facts.len());
        for f in facts.iter().take(3) {
            let content: String = f
                .payload
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .chars()
                .take(56)
                .collect();
            let neg = if f.payload["negated"].as_bool().unwrap_or(false) {
                " (neg)"
            } else {
                ""
            };
            println!("      · {content}{neg}");
        }
    }

    // ── Stage 3: the listener (user) side also accumulates facts ────────
    println!("\n── 阶段 3：倾听者（user）实体画像 ──");
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
        "皮埃尔's persona must chat out via agent_personality facts, got {}",
        persona_facts.len()
    );
    assert!(
        by_facet.len() >= 3,
        "皮埃尔's persona should span >=3 facets, got {by_facet:?}"
    );
    println!(
        "\n  ✓ 断言通过：皮埃尔人格被聊出来了（{} 条，跨 {} 类）",
        persona_facts.len(),
        by_facet.len()
    );
    println!();
}
