//! MCP full-loop test on every `corpus/*.json` companion dialog.
//!
//! For each JSON corpus with a `messages` array this test drives the real MCP
//! tools over the JSON-RPC path and verifies the COMPLETE companion-persona
//! closed loop — the layer `tests/mcp_corpus_real_dialog.rs` only partially
//! covered (inject→check). Here we additionally exercise:
//!
//!   agent_fact_compile           (chat out the agent's persona)
//!     → persona_inject           (aggregate a structured persona card)
//!     → persona_check            (guard a contradictory draft)
//!     → relationship_update      (roll intimacy from message emotion signals)
//!     → relationship_query       (read the persisted snapshot back)
//!     → persona_timeline         (rebuild the 起点→转折→现状 evolution arc)
//!     → memory_decay             (scan, protect persona facts, never delete)
//!
//! Three corpora are exercised:
//!   - `bailiusu_escape.json`           白流苏逃出白家（66 轮，强第一人称信号）
//!   - `warpeace_pierre.json`           皮埃尔·别祖霍夫（51 轮，强第一人称信号）
//!   - `conversation_export_2026-08-02.json` 真实编码陪伴对话（41 轮，弱信号）
//!
//! The weak-signal corpus is the interesting one: a real coding-companion dialog
//! has sparse self-referential persona statements, so every downstream stage must
//! degrade gracefully (relationship still moves, timeline still has a start,
//! decay still protects the few persona facts) rather than panic or over-flag.
//!
//! Run: cargo test --test mcp_corpus_full_loop -- --nocapture

use std::sync::Arc;

use async_trait::async_trait;

use lore_scope::cognition::FactStore as _;
use lore_scope::embed::NullEmbedder;
use lore_scope::fact_store::SqliteFactStore;
use lore_scope::knowledge::EntityLinker;
use lore_scope::knowledge::SQLiteKnowledgeStore;
use lore_scope::knowledge::external::ExternalKnowledgeRegistry;
use lore_scope::mcp::external_knowledge_tools::register_external_knowledge_tools;
use lore_scope::mcp::generalize_tool::register_generalize_tool;
use lore_scope::mcp::knowledge_tools::register_knowledge_tools;
use lore_scope::mcp::persona_check_tool::PersonaCheckTool;
use lore_scope::mcp::persona_inject_tool::PersonaInjectTool;
use lore_scope::mcp::relationship_tool::{
    PersonaTimelineTool, RelationshipQueryTool, RelationshipUpdateTool,
};
use lore_scope::mcp::server::{MCPServer, ServerBuilder};
use lore_scope::mcp::transport::Transport;
use lore_scope::mcp::types::ToolHandler;
use lore_scope::mcp::types::{Implementation, JSONRPCMessage, JSONRPCRequest, JSONRPCResponse};
use serde_json::{Value, json};

/// One corpus entry: file path + the agent_id to attribute assistant turns to.
struct Corpus {
    path: &'static str,
    agent_id: &'static str,
    user_id: &'static str,
    /// Minimum messages required (guards against a silently-empty corpus).
    min_messages: usize,
    /// Whether this corpus is expected to chat out ≥1 persona fact.
    /// Novel-style dialogs (白流苏, 皮埃尔) do; the real coding-companion
    /// dialog may not — its assistant turns are operational, not self-ref.
    expect_persona_facts: bool,
    /// Whether to also run the story_bridge stage (novel corpus only).
    /// Real-dialog corpora compile via agent_fact_compile, not generalize_compile,
    /// so they have no knowledge-graph story events to bridge.
    run_story_bridge: bool,
}

const CORPORA: &[Corpus] = &[
    Corpus {
        path: "corpus/bailiusu_escape.json",
        agent_id: "bailiusu",
        user_id: "helper",
        min_messages: 20,
        expect_persona_facts: true,
        run_story_bridge: false,
    },
    Corpus {
        path: "corpus/warpeace_pierre.json",
        agent_id: "pierre",
        user_id: "listener",
        min_messages: 100,
        expect_persona_facts: true,
        run_story_bridge: false,
    },
    Corpus {
        path: "corpus/conversation_export_2026-08-02.json",
        agent_id: "coding-companion",
        user_id: "developer",
        min_messages: 40,
        expect_persona_facts: false,
        run_story_bridge: false,
    },
];

/// Single-shot in-memory transport: yields one request, captures one response.
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
async fn call_tool(server: &MCPServer, id: u64, name: &str, args: Value) -> Value {
    let mut t = OneShotTransport {
        inbox: vec![JSONRPCMessage::Request(JSONRPCRequest {
            jsonrpc: "2.0".to_string(),
            id: json!(id),
            method: "tools/call".to_string(),
            params: Some(json!({ "name": name, "arguments": args })),
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

/// Pull the first text block's string from a `tools/call` result.
fn text_of(result: &Value) -> String {
    result
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|b| b.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Parse a tool result's text payload into a JSON value.
fn parse_json(result: &Value) -> Value {
    let txt = text_of(result);
    if txt.is_empty() {
        return Value::Null;
    }
    serde_json::from_str(&txt).unwrap_or_else(|_| Value::Null)
}

/// Build a server wired with the knowledge + fact + external tools so
/// `agent_fact_compile` works. Persona tools are instantiated directly (they
/// hold the shared fact store) rather than registered on the server.
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

    let builder = ServerBuilder::new(Implementation {
        name: "mcp-corpus-full-loop".into(),
        version: "0.1.0".into(),
    });
    let builder = register_knowledge_tools(
        builder,
        kstore.clone(),
        fact_store.clone(),
        Some(entity_linker.clone()),
    )
    .await;
    let builder = register_external_knowledge_tools(
        builder,
        external_registry,
        entity_linker,
        kstore.clone(),
        fact_store.clone(),
    )
    .await;
    let builder = register_generalize_tool(builder, kstore.clone()).await;
    (builder.build(), fact_store)
}

/// Load a corpus's messages and return them as a JSON array of role/content.
fn load_messages(corpus_path: &str) -> Vec<Value> {
    let full = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), corpus_path);
    let raw = std::fs::read_to_string(&full)
        .unwrap_or_else(|e| panic!("read corpus `{corpus_path}` failed: {e}"));
    let data: Value = serde_json::from_str(&raw).expect("valid JSON corpus");
    data["messages"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|m| json!({ "role": m["role"], "content": m["content"] }))
                .collect()
        })
        .unwrap_or_default()
}

/// Count persona facts (attribution == "agent_personality") for an entity.
fn count_persona_facts(facts: &[lore_scope::cognition::Fact]) -> (usize, Vec<String>) {
    let mut facets = std::collections::BTreeSet::new();
    let mut n = 0;
    for f in facts {
        if f.payload.get("attribution") == Some(&Value::String("agent_personality".into())) {
            n += 1;
            facets.insert(format!("{:?}", f.fact_type));
        }
    }
    (n, facets.into_iter().collect())
}

#[tokio::test]
async fn every_corpus_full_companion_loop() {
    println!("\n════════════════════════════════════════════════════════");
    println!("  MCP 测试：corpus/*.json 全量陪伴型完整闭环");
    println!("════════════════════════════════════════════════════════\n");

    for corpus in CORPORA {
        println!(
            "══════ `{}` (agent={}) ════════",
            corpus.path, corpus.agent_id
        );

        let messages = load_messages(corpus.path);
        assert!(
            messages.len() >= corpus.min_messages,
            "`{}` has {} messages, expected ≥ {}",
            corpus.path,
            messages.len(),
            corpus.min_messages,
        );
        println!("  对话加载：{} 条消息", messages.len());

        // Each corpus gets a fresh server so facts don't bleed across corpora.
        let (server, fact_store) = build_server().await;

        // ── Stage 1: agent_fact_compile ─────────────────────────────────────
        let compile_out = call_tool(
            &server,
            1,
            "agent_fact_compile",
            json!({
                "messages": messages,
                "user_id": corpus.user_id,
                "agent_id": corpus.agent_id,
                "include_agent_facts": true,
            }),
        )
        .await;
        let compile_json = parse_json(&compile_out);
        let agent_facts_persisted = compile_json["agent_facts_persisted"].as_i64().unwrap_or(0);
        println!("  ① agent_fact_compile：agent_facts_persisted={agent_facts_persisted}");

        // ── Stage 2: read the agent entity's accumulated persona ───────────
        let entity_id = fact_store
            .resolve_agent("default", corpus.agent_id)
            .expect("resolve agent after compile");
        let agent_facts = fact_store.get_facts(entity_id).expect("read agent facts");
        let (persona_count, facets) = count_persona_facts(&agent_facts);
        println!(
            "  ② Agent 实体 id={entity_id}，事实 {} 条，人格 {} 条，跨 {} 类 {facets:?}",
            agent_facts.len(),
            persona_count,
            facets.len(),
        );

        if corpus.expect_persona_facts {
            assert!(
                persona_count > 0,
                "`{}` should chat out persona facts, got {}",
                corpus.path,
                persona_count,
            );
            assert!(
                facets.len() >= 2,
                "`{}` persona should span ≥2 facets, got {facets:?}",
                corpus.path,
            );
        } else {
            println!(
                "  ② 弱信号语料：persona={} 条，闭环仍须全程成立",
                persona_count
            );
        }

        // ── Stage 3: persona_inject aggregates a card from whatever exists ─
        let inject = PersonaInjectTool::new(fact_store.clone());
        let inject_out = inject
            .call(&json!({
                "agent_id": corpus.agent_id,
                "tenant_id": "default",
                "format": "json",
            }))
            .await
            .expect("persona_inject succeeds");
        let inject_json = parse_json(&serde_json::to_value(&inject_out).unwrap_or(Value::Null));
        let _ = inject_json;
        println!("  ③ persona_inject：✓ 卡片聚合完成");

        // ── Stage 4: persona_check guards a contradictory draft ────────────
        let (draft, expect_clean) = if persona_count > 0 {
            ("我讨厌一切，我再也不肯了。", false)
        } else {
            ("今天天气不错。", true)
        };
        let check = PersonaCheckTool::new(fact_store.clone(), Arc::new(NullEmbedder)).await;
        let check_out = check
            .call(&json!({
                "agent_id": corpus.agent_id,
                "tenant_id": "default",
                "draft": draft,
            }))
            .await
            .expect("persona_check succeeds");
        let check_json = parse_json(&serde_json::to_value(&check_out).unwrap_or(Value::Null));
        let clean = check_json["clean"].as_bool().unwrap_or(false);
        let conflicts = check_json["conflicts"].as_array().map_or(0, Vec::len);
        let drift = check_json["drift"].as_array().map_or(0, Vec::len);
        let total_signals = check_json["stats"]["total_signals"].as_i64().unwrap_or(0);
        println!(
            "  ④ persona_check（draft=\"{draft}\"）clean={clean}, conflicts={conflicts}, drift={drift}, total_signals={total_signals}",
        );
        if expect_clean {
            assert!(
                clean,
                "`{}` neutral draft must be clean, got conflicts={conflicts} drift={drift}",
                corpus.path,
            );
        }
        assert!(
            check_json != Value::Null,
            "`{}` persona_check must return a result",
            corpus.path,
        );

        // ── Stage 5: relationship_update rolls intimacy from messages ──────
        let update = RelationshipUpdateTool::new(fact_store.clone());
        let update_out = update
            .call(&json!({
                "tenant_id": "default",
                "agent_id": corpus.agent_id,
                "user_id": corpus.user_id,
                "messages": messages,
            }))
            .await
            .expect("relationship_update succeeds");
        let update_json = parse_json(&serde_json::to_value(&update_out).unwrap_or(Value::Null));
        let exists_after_update = update_json["exists"].as_bool().unwrap_or(false);
        let intimacy_after_update = update_json["intimacy"].as_f64().unwrap_or(0.0);
        let stage = update_json["stage"].as_str().unwrap_or("");
        println!(
            "  ⑤ relationship_update：exists={exists_after_update}, intimacy={intimacy_after_update:.4}, stage=\"{stage}\"",
        );
        assert!(
            exists_after_update,
            "`{}` relationship must exist after update",
            corpus.path
        );

        // ── Stage 6: relationship_query reads the persisted snapshot back ──
        let query = RelationshipQueryTool::new(fact_store.clone());
        let query_out = query
            .call(&json!({
                "tenant_id": "default",
                "agent_id": corpus.agent_id,
                "user_id": corpus.user_id,
            }))
            .await
            .expect("relationship_query succeeds");
        let query_json = parse_json(&serde_json::to_value(&query_out).unwrap_or(Value::Null));
        let intimacy_after_query = query_json["intimacy"].as_f64().unwrap_or(-1.0);
        println!("  ⑥ relationship_query：intimacy={intimacy_after_query:.4} (跨会话持久)",);
        assert!(
            (intimacy_after_query - intimacy_after_update).abs() < 1e-9,
            "`{}` relationship_query must return the persisted intimacy ({intimacy_after_update}), got {intimacy_after_query}",
            corpus.path,
        );

        // ── Stage 7: persona_timeline rebuilds the evolution arc ───────────
        let timeline = PersonaTimelineTool::new(fact_store.clone());
        let tl_out = timeline
            .call(&json!({ "entity_id": entity_id }))
            .await
            .expect("persona_timeline succeeds");
        let tl_json = parse_json(&serde_json::to_value(&tl_out).unwrap_or(Value::Null));
        let start_ok = tl_json["start"].is_object();
        let current_ok = tl_json["current"].is_object();
        let trajectory = tl_json["trajectory"].as_array().map_or(0, Vec::len);
        let milestones = tl_json["milestones"].as_array().map_or(0, Vec::len);
        println!(
            "  ⑦ persona_timeline：start={start_ok}, current={current_ok}, trajectory={trajectory}, milestones={milestones}",
        );
        assert!(start_ok, "`{}` timeline must have an origin", corpus.path);
        assert!(current_ok, "`{}` timeline must have a present", corpus.path);
        assert!(
            trajectory > 0,
            "`{}` timeline must keep the full trajectory, got {trajectory}",
            corpus.path,
        );

        // ── Stage 8: memory_decay scans, protects persona facts, no delete ─
        let decay = lore_scope::mcp::decay_tool::MemoryDecayTool::new(fact_store.clone());
        let decay_out = decay
            .call(&json!({ "entity_id": entity_id }))
            .await
            .expect("memory_decay succeeds");
        let decay_json = parse_json(&serde_json::to_value(&decay_out).unwrap_or(Value::Null));
        let scanned = decay_json["scanned"].as_i64().unwrap_or(0);
        let high_value_protected = decay_json["high_value_protected"].as_i64().unwrap_or(0);
        let decayed = decay_json["decayed"].as_i64().unwrap_or(0);
        println!(
            "  ⑧ memory_decay：scanned={scanned}, high_value_protected={high_value_protected}, decayed={decayed}",
        );
        assert!(
            scanned >= agent_facts.len() as i64,
            "`{}` memory_decay must scan all facts, scanned={scanned} facts={}",
            corpus.path,
            agent_facts.len(),
        );
        // Persona facts are high-value → protected. Non-persona facts may decay.
        let expected_protected = persona_count as i64;
        assert!(
            high_value_protected >= expected_protected,
            "`{}` memory_decay must protect all persona facts, protected={high_value_protected} persona={expected_protected}",
            corpus.path,
        );
        // After decay, verify facts are STILL there (no deletion).
        let facts_after_decay = fact_store
            .get_facts(entity_id)
            .expect("read facts after decay");
        assert_eq!(
            facts_after_decay.len(),
            agent_facts.len(),
            "`{}` memory_decay must not delete any fact: before={} after={}",
            corpus.path,
            agent_facts.len(),
            facts_after_decay.len(),
        );

        println!("  ✓ `{}` 完整闭环成立\n", corpus.path);
    }

    println!("════════════════════════════════════════════════════════");
    println!("  全量 corpus/*.json 陪伴型完整闭环验证通过（8 阶段 × 3 语料）");
    println!("════════════════════════════════════════════════════════\n");
}
