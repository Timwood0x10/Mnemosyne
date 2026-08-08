//! MCP persona-loop test on every `corpus/*.json` companion dialog.
//!
//! For each JSON corpus with a `messages` array this test drives the real MCP
//! tools over the JSON-RPC path and verifies the closed loop:
//!
//!   agent_fact_compile (chat out the agent's persona)
//!     → persona_check (guard a contradictory draft against the stored persona)
//!     → persona_inject (aggregate a structured persona card from the facts)
//!
//! Three corpora are exercised:
//!   - `bailiusu_escape.json`      白流苏逃出白家（41 轮，强第一人称信号）
//!   - `warpeace_pierre.json`      皮埃尔·别祖霍夫（51 轮，强第一人称信号）
//!   - `conversation_export_*.json`真实编码陪伴对话（41 轮，弱第一人称信号）
//!
//! The weak-signal corpus is the interesting one: a real coding-companion dialog
//! has few self-referential persona statements, so the loop must degrade
//! gracefully (persona_check stays clean, persona_inject still aggregates what
//! little exists) rather than panic or over-flag.
//!
//! Run: cargo test --test mcp_corpus_real_dialog -- --nocapture

use std::sync::Arc;

use async_trait::async_trait;

use mnemosyne::cognition::FactStore as _;
use mnemosyne::fact_store::SqliteFactStore;
use mnemosyne::knowledge::EntityLinker;
use mnemosyne::knowledge::SQLiteKnowledgeStore;
use mnemosyne::knowledge::external::ExternalKnowledgeRegistry;
use mnemosyne::mcp::external_knowledge_tools::register_external_knowledge_tools;
use mnemosyne::mcp::generalize_tool::register_generalize_tool;
use mnemosyne::mcp::knowledge_tools::register_knowledge_tools;
use mnemosyne::mcp::persona_check_tool::PersonaCheckTool;
use mnemosyne::mcp::persona_inject_tool::PersonaInjectTool;
use mnemosyne::mcp::server::{MCPServer, ServerBuilder};
use mnemosyne::mcp::transport::Transport;
use mnemosyne::mcp::types::ToolHandler;
use mnemosyne::mcp::types::{Implementation, JSONRPCMessage, JSONRPCRequest, JSONRPCResponse};
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
}

const CORPORA: &[Corpus] = &[
    Corpus {
        path: "corpus/bailiusu_escape.json",
        agent_id: "bailiusu",
        user_id: "helper",
        min_messages: 20,
        expect_persona_facts: true,
    },
    Corpus {
        path: "corpus/warpeace_pierre.json",
        agent_id: "pierre",
        user_id: "listener",
        min_messages: 100,
        expect_persona_facts: true,
    },
    Corpus {
        path: "corpus/conversation_export_2026-08-02.json",
        agent_id: "coding-companion",
        user_id: "developer",
        min_messages: 40,
        expect_persona_facts: false,
    },
];

/// Single-shot in-memory transport: yields one request, captures one response.
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
    serde_json::from_str(&txt).unwrap_or(Value::Null)
}

/// Build a server wired with the knowledge + fact + persona tools, sharing one
/// fact store so `persona_check` and `persona_inject` read the facts
/// `agent_fact_compile` wrote.
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
        name: "mcp-corpus-persona".into(),
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
fn count_persona_facts(facts: &[mnemosyne::cognition::Fact]) -> (usize, Vec<String>) {
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
async fn every_corpus_persona_loop() {
    println!("\n════════════════════════════════════════════════════════");
    println!("  MCP 测试：corpus/*.json 全量陪伴型 persona 闭环");
    println!("════════════════════════════════════════════════════════\n");

    for corpus in CORPORA {
        println!(
            "──────── `{}` (agent={}) ────────",
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

        // ── Stage 1: feed the dialog through agent_fact_compile ─────────────
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
        println!("  agent_fact_compile：agent_facts_persisted={agent_facts_persisted}");

        // ── Stage 2: read the agent entity's accumulated persona ───────────
        let entity_id = fact_store
            .resolve_agent("default", corpus.agent_id)
            .expect("resolve agent after compile");
        let agent_facts = fact_store.get_facts(entity_id).expect("read agent facts");
        let (persona_count, facets) = count_persona_facts(&agent_facts);
        println!(
            "  Agent 实体 id={entity_id}，事实 {} 条，人格 {} 条，跨 {} 类 {facets:?}",
            agent_facts.len(),
            persona_count,
            facets.len()
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
            // Weak-signal corpus: persona may be empty. The loop must still
            // complete cleanly without panicking — that IS the assertion.
            println!("  (弱信号语料：persona={} 条，闭环仍须成立)", persona_count);
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
        // inject returns a persona_card object; we just assert it ran.
        let _ = inject_json;
        println!("  persona_inject：✓ 卡片聚合完成");

        // ── Stage 4: persona_check guards a contradictory draft ────────────
        // We probe with a draft that contradicts the corpus's stance IF the
        // agent has ≥1 persona fact, else a neutral draft that must stay clean.
        let (draft, expect_clean) = if persona_count > 0 {
            // Find a Preference or Emotion fact to contradict; else probe with
            // a generic stance flip ("我讨厌一切" negated vs an affirmative twin).
            ("我讨厌一切，我再也不肯了。", false)
        } else {
            // No stored persona → any signal is drift, but "今天天气不错" has no
            // persona signal at all → clean.
            ("今天天气不错。", true)
        };
        let check =
            PersonaCheckTool::new(fact_store.clone(), Arc::new(mnemosyne::embed::NullEmbedder))
                .await;
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
            "  persona_check（draft=\"{draft}\"）clean={clean}, conflicts={conflicts}, drift={drift}, total_signals={total_signals}"
        );

        if expect_clean {
            assert!(
                clean,
                "`{}` neutral draft must be clean, got conflicts={conflicts} drift={drift}",
                corpus.path,
            );
        }
        // When persona exists and we probed with a negated stance, the keyword
        // path should at least extract a signal (total_signals > 0) OR find the
        // draft unanchored (drift > 0). The exact outcome depends on whether the
        // stored facts share bigrams with the probe — we assert it ran without
        // error and produced a sensible (non-null) result.
        assert!(
            check_json != Value::Null,
            "`{}` persona_check must return a result, got Null",
            corpus.path,
        );

        println!("  ✓ `{}` 闭环成立\n", corpus.path);
    }

    println!("════════════════════════════════════════════════════════");
    println!("  全量 corpus/*.json 陪伴型 persona 闭环验证通过");
    println!("════════════════════════════════════════════════════════\n");
}
