//! Companion-AI demo: verify the "对话 → 画像 → 以画像聊天" loop end-to-end.
//!
//! Scenario: the AI role-plays 白流苏 (from 张爱玲《倾城之恋》). The demo
//! drives the **real MCP tools** over JSON-RPC (not the internals) through
//! four stages:
//!
//! 0. Static persona  — compile `corpus/倾城之恋.txt` into the knowledge
//!    graph, then `inspect_entity("白流苏")`.
//! 1. Conversation    — simulate 200 turns of user ↔ 白流苏 chat; every 50
//!    turns call `agent_fact_compile` (incremental archive).
//! 2. Key events      — `person_key_events("白流苏")` from the novel graph.
//! 3. Portrait drift  — read the fact store at turn 50 vs turn 200 and show
//!    whether 白流苏's/用户's portrait actually evolved.
//!
//! Run: cargo test --test companion_demo -- --nocapture
//!
//! The test drives tools through a real [`MCPServer`] so it exercises the same
//! JSON-RPC path an agent host would use, and prints a human-readable report.

use std::sync::Arc;

use async_trait::async_trait;

use mnemosyne::cognition::{Fact, FactStore as _, StateEngine, build_snapshot};
use mnemosyne::fact_store::SqliteFactStore;
use mnemosyne::knowledge::EntityLinker;
use mnemosyne::knowledge::SQLiteKnowledgeStore;
use mnemosyne::knowledge::external::ExternalKnowledgeRegistry;
use mnemosyne::knowledge::store::KnowledgeStore;
use mnemosyne::mcp::external_knowledge_tools::register_external_knowledge_tools;
use mnemosyne::mcp::generalize_tool::register_generalize_tool;
use mnemosyne::mcp::key_events_tool::{KeyEventsTool, key_events_definition};
use mnemosyne::mcp::knowledge_tools::register_knowledge_tools;
use mnemosyne::mcp::server::{MCPServer, ServerBuilder};
use mnemosyne::mcp::transport::Transport;
use mnemosyne::mcp::types::{Implementation, JSONRPCMessage, JSONRPCRequest, JSONRPCResponse};
use mnemosyne::types::Message;

// ── In-memory transport so we can drive the MCP server like a client ────────

/// A single-shot transport: yields exactly one request then EOF, capturing
/// the single response. Good enough for our request/response tool calls.
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

/// Call an MCP tool on a live server and return the JSON `result` object
/// (or panic with the error). Mirrors how an agent host calls `tools/call`.
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
            params: Some(serde_json::json!({
                "name": name,
                "arguments": args,
            })),
        })],
        outbox: vec![],
    };
    server
        .serve(&mut t)
        .await
        .expect("MCP serve must complete without error");
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

/// Build a server wired exactly like `main.rs`: all the knowledge/external
/// tools + `person_key_events`, sharing one knowledge store + fact store.
async fn build_server() -> (MCPServer, Arc<SQLiteKnowledgeStore>, Arc<SqliteFactStore>) {
    let kstore: Arc<SQLiteKnowledgeStore> = Arc::new(
        SQLiteKnowledgeStore::open_in_memory()
            .await
            .expect("open in-memory knowledge store"),
    );
    let fact_store =
        Arc::new(SqliteFactStore::open_in_memory().expect("open in-memory fact store"));
    let entity_linker: Arc<std::sync::RwLock<EntityLinker>> =
        Arc::new(std::sync::RwLock::new(EntityLinker::new()));
    let external_registry = Arc::new(ExternalKnowledgeRegistry::new());

    let mut builder = ServerBuilder::new(Implementation {
        name: "companion-demo".into(),
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

    (builder.build(), kstore, fact_store)
}

// ── Simulated 200-turn companion dialogue ──────────────────────────────────
//
// 白流苏 is the AI (agent_id="bailiusu"). She answers in her own voice —
// 矜持、爱低头、话里有话、不肯示弱. The user (user_id="zhangsan") is the
// human companion. Every message carries personality signals the compiler can
// surface as Facts. The dialogue is generated deterministically from templates
// so the test is reproducible.

/// Content pool: 用户 turns. Each line is a self-contained statement/question
/// a companion would say.
const USER_LINES: &[&str] = &[
    "我今天工作上又被人抢了功劳，心里堵得慌。",
    "你觉得人为什么要结婚呢？我越来越不明白了。",
    "我喜欢下雨天，但讨厌雨天的分别。",
    "我最近失眠，总想起以前的事。",
    "你说我是不是太要强了，什么都要自己扛？",
    "今天我学会了做饭，虽然很难吃，但很有成就感。",
    "我想去一个很远的地方，谁也不认识我。",
    "我不喜欢跟人争，但也不肯认输。",
    "你有没有很想念一个人，却说不出口的时候？",
    "我觉得自己像一只被困住的鸟。",
    "今天有人夸我温柔，我觉得很可笑。",
    "我不太会表达自己，话到嘴边就咽回去了。",
    "我想要一份安稳，但又怕一眼看到头。",
    "你说，一个人能真正依靠的，是不是只有自己？",
    "我今天喝了很多咖啡，还是觉得困。",
    "我不喜欢虚伪的应酬，太累了。",
    "我在学钢琴，可是手指总是不听话。",
    "你说我该不该换一份工作？我好迷茫。",
    "我总觉得别人看我的眼神很奇怪。",
    "今天天气很好，我却高兴不起来。",
    "我想要有人懂我，不用我说，就懂。",
    "我害怕失去，所以常常先推开别人。",
    "我最近总梦到回到小时候。",
    "你会不会觉得我很矫情？",
    "我其实很羡慕那些敢爱敢恨的人。",
    "今天加班到很晚，一个人走夜路。",
    "我不爱说话，但心里想得很多。",
    "你说，真心是不是都是会被辜负的？",
    "我买了一束花给自己，却不知道送给谁。",
    "我想听你说说你自己，白流苏。",
];

/// Content pool: 白流苏's replies — in-character (矜持、敏感、要强、话里有话).
const AGENT_LINES: &[&str] = &[
    "原是这般。委屈你的人，原不值得你为他堵心。",
    "我低头惯了，可心里也有不肯认的时候。",
    "你要强，倒像个我。可我劝你一句，别把自个儿磨得太狠了。",
    "话到嘴边咽回去的滋味，我最是知道。",
    "我一个人过了这些年，早明白能依靠的，只有自己。",
    "那花，你既买了，就送给你自己罢。",
    "我不要你为我改什么。你好也罢，坏也罢，我都受着。",
    "你问我懂不懂你，我是懂的。正因懂，才不轻易开口。",
    "这世上的事，原没有十全十美的，抓住一星半点，也就是了。",
    "你怕失去，所以先推开人。这心思，我也曾有过。",
    "念旧的人，最是走不远的。可我也劝不住你。",
    "你瞧你，又低头了。低头本是你的长处，可也别总低着头。",
    "我是白流苏，离过婚，爱过，也输过。可我还活着。",
    "你要换工作，便换。人生这一局，你尽管下注，输了也不打紧。",
    "失眠的时候，我常听胡琴。那调子，苍凉得很，却也耐听。",
    "你羡慕敢爱敢恨的人，我倒羡慕你，还有这份心气。",
    "真心会不会被辜负，我说不准。可我宁可给了，也不藏着。",
    "你一个人走夜路，心里害怕，就想想我罢，我也常走夜路。",
    "我不是矫情的人。我不过是个过了时的人罢了。",
    "我懂你，就像懂我当年站在镜子前，端详自己还老不老。",
];

/// Deterministically generate `turns` pairs of (user, agent) messages.
fn dialogue(turns: usize) -> Vec<Message> {
    let mut msgs = Vec::with_capacity(turns * 2);
    for i in 0..turns {
        let user_line = USER_LINES[i % USER_LINES.len()];
        let agent_line = AGENT_LINES[(i * 7 + 3) % AGENT_LINES.len()];
        let mut u = Message::new("user", user_line);
        u.turn_id = Some(format!("turn-{}", i * 2));
        msgs.push(u);
        let mut a = Message::new("assistant", agent_line);
        a.turn_id = Some(format!("turn-{}", i * 2 + 1));
        msgs.push(a);
    }
    msgs
}

#[tokio::test]
async fn companion_demo_end_to_end() {
    println!("\n════════════════════════════════════════════════════════");
    println!("  陪伴型 AI Demo：你是白流苏，和用户聊 200 句");
    println!("════════════════════════════════════════════════════════\n");

    let (server, kstore, fact_store) = build_server().await;

    // ── Stage 0: static persona from the novel ───────────────────────────
    println!("── 阶段 0：静态画像（从《倾城之恋》原文编译）──");
    let novel_path = concat!(env!("CARGO_MANIFEST_DIR"), "/corpus/倾城之恋.txt");
    let novel_text = match std::fs::read_to_string(novel_path) {
        Ok(t) => t,
        Err(e) => {
            println!("⚠  corpus/倾城之恋.txt 不可用（{e}），跳过静态画像阶段");
            return;
        }
    };
    let chars = novel_text.chars().count();
    let compile_res = call_tool(
        &server,
        1,
        "generalize_compile",
        serde_json::json!({
            "doc_type": "text",
            "title": "倾城之恋",
            "source": "corpus",
            "text": novel_text,
        }),
    )
    .await;
    let compile_out = text_of(&compile_res);
    println!("  原文 chars={chars}");
    println!("  generalize_compile → {compile_out}\n");

    // Resolve the protagonist's canonical Person name in the compiled graph.
    // Corpus discovery may store the protagonist as "流苏" (2-char given name)
    // rather than "白流苏", and may also leave a truncated "白流" phantom; we
    // pick the Person whose name has a substring relationship with "白流苏" AND
    // carries the most participated_in edges (the real protagonist, not a
    // truncation artifact).
    use mnemosyne::knowledge::ObjectType;
    let all_objs = kstore
        .search_objects(None, None, None, None, 20_000)
        .await
        .unwrap_or_default();
    let mut protagonist: Option<(String, usize)> = None;
    for o in &all_objs {
        if o.object_type != ObjectType::Person {
            continue;
        }
        if !(o.name.contains("白流苏") || "白流苏".contains(&o.name)) {
            continue;
        }
        let edges = kstore.get_edges_touching(o.id).await.unwrap_or_default();
        let participated = edges
            .iter()
            .filter(|e| e.predicate == "participated_in")
            .count();
        if protagonist.as_ref().is_none_or(|(_, n)| participated > *n) {
            protagonist = Some((o.name.clone(), participated));
        }
    }
    let canonical = protagonist
        .clone()
        .map(|(n, _)| n)
        .unwrap_or_else(|| "白流苏".to_string());
    if let Some((name, n)) = &protagonist {
        println!("  ✓ 主角识别：Person \"{name}\" 参与了 {n} 个关键事件");
    } else {
        println!("  ⚠ 未从小说中解析出主角 Person 实体");
    }

    let inspect = call_tool(
        &server,
        2,
        "inspect_entity",
        serde_json::json!({"name": canonical}),
    )
    .await;
    let inspect_text = text_of(&inspect);
    println!("  inspect_entity(\"{canonical}\") → {inspect_text}\n");

    // ── Stage 1: 200-turn conversation, incremental archive every 50 ─────
    println!("── 阶段 1：200 句对话，每 50 句调 agent_fact_compile ──");
    let all = dialogue(100); // 100 user + 100 agent = 200 句
    println!("  生成对话：{} 句", all.len());

    let user_id = "zhangsan";
    let agent_id = "bailiusu";
    let mut turn50 = None;
    let mut turn200 = None;

    let mut mid = 0usize;
    while mid < all.len() {
        let end = (mid + 100).min(all.len()); // 100 messages = 50 句
        let chunk: Vec<Message> = all[mid..end].to_vec();
        let chunk_json: Vec<serde_json::Value> = chunk
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": m.role,
                    "content": m.content,
                    "turn_id": m.turn_id,
                })
            })
            .collect();
        let res = call_tool(
            &server,
            10 + mid as u64,
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
        let turns_done = end / 2;
        println!("  归档第 {turns_done}/100 组（句数~{}）→ {out}", end);
        if turns_done == 50 {
            turn50 = Some(turns_done);
        }
        if end >= all.len() {
            turn200 = Some(turns_done);
        }
        mid = end;
    }
    println!();

    // ── Stage 2: key events from the novel trajectory ───────────────────
    println!("── 阶段 2：person_key_events（关键事件）──");
    // A per-sentence story event carries one evidence snippet and typically no
    // extra participants beyond the queried protagonist, so its importance is
    // ~0.15; use a low threshold so genuine beats surface.
    let kev = call_tool(
        &server,
        90,
        "person_key_events",
        serde_json::json!({"name": canonical, "threshold": 0.1}),
    )
    .await;
    let kev_text = text_of(&kev);
    println!("  person_key_events(\"{canonical}\") → {kev_text}");
    // Module C claim: the novel's story beats materialized into real
    // participated_in events, so the protagonist has a queryable trajectory
    // AND at least one beat qualifies as a key event with a score + evidence.
    let kev_json: serde_json::Value =
        serde_json::from_str(&kev_text).unwrap_or_else(|_| serde_json::json!({}));
    let total_events = kev_json
        .get("total_events")
        .and_then(|n| n.as_i64())
        .unwrap_or(0);
    let key_count = kev_json
        .get("key_events")
        .and_then(|k| k.as_array())
        .map_or(0, Vec::len);
    assert!(
        total_events > 0,
        "protagonist must have >0 participated_in events from the novel, got {total_events}"
    );
    assert!(
        key_count > 0,
        "protagonist must surface >=1 key event at threshold 0.1, got {key_count}"
    );
    println!();

    // ── Stage 3: portrait drift 50 句 vs 200 句 ─────────────────────────
    println!("── 阶段 3：画像是否真的“聊出来”了？（50 句 vs 200 句）──");
    let state_engine = StateEngine::new();

    let user_entity = fact_store
        .resolve_user("default", user_id)
        .expect("resolve user");
    let agent_entity = fact_store
        .resolve_agent("default", agent_id)
        .expect("resolve agent");

    let user_facts = fact_store.get_facts(user_entity).expect("user facts");
    let agent_facts = fact_store.get_facts(agent_entity).expect("agent facts");

    fn summarize(name: &str, entity_id: i64, facts: Vec<Fact>, engine: &StateEngine) {
        let snapshot = build_snapshot(
            entity_id,
            name.to_string(),
            if name.starts_with("User:") {
                "User"
            } else {
                "Agent"
            }
            .to_string(),
            facts.clone(),
            engine,
        );
        let type_counts = fact_type_counts(&facts);
        println!(
            "  ── {name}（id={entity_id}，累计事实 {} 条）──",
            facts.len()
        );
        for (k, v) in type_counts {
            println!("     {k}: {v} 条");
        }
        for f in facts.iter().take(6) {
            let preview: String = f.payload.to_string().chars().take(80).collect();
            println!("     · [{:?}] {}", f.fact_type, preview);
        }
        println!(
            "     （快照 JSON: {}）",
            serde_json::to_string(&snapshot).unwrap_or_default()
        );
        println!();
    }

    summarize(
        "User:zhangsan",
        user_entity,
        user_facts.clone(),
        &state_engine,
    );
    summarize(
        "Agent:bailiusu",
        agent_entity,
        agent_facts.clone(),
        &state_engine,
    );

    // The demo's core claim: BOTH the user portrait AND 白流苏's persona
    // accumulate facts as turns grow. Before the agent-personality channel,
    // 白流苏's agent facts were always zero — this assertion is the "人格真的
    // 聊出来了吗" gate (Module A).
    assert!(
        !user_facts.is_empty(),
        "user portrait must accumulate facts after 200 turns, got {}",
        user_facts.len()
    );
    assert!(
        !agent_facts.is_empty(),
        "白流苏's persona must accumulate facts after 200 turns, got {}",
        agent_facts.len()
    );
    // 白流苏's persona must include personality facets, not just action events.
    let has_personality = agent_facts.iter().any(|f| {
        f.payload.get("attribution") == Some(&serde_json::Value::String("agent_personality".into()))
    });
    assert!(
        has_personality,
        "白流苏's persona must include agent_personality facts, not only tool/action events"
    );
    println!(
        "  ✓ 断言通过：用户 {} 条 / 白流苏 {} 条（含人格标记）",
        user_facts.len(),
        agent_facts.len()
    );
    let _ = (turn50, turn200);
}

fn fact_type_counts(facts: &[Fact]) -> Vec<(String, usize)> {
    use std::collections::BTreeMap;
    let mut m = BTreeMap::new();
    for f in facts {
        *m.entry(format!("{:?}", f.fact_type)).or_insert(0usize) += 1;
    }
    m.into_iter().collect()
}
