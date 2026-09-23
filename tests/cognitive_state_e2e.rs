//! End-to-end verification of the cognition layer over the REAL MCP JSON-RPC
//! path.
//!
//! Every other integration test in this repository calls a `ToolHandler`
//! directly, which skips the server's method dispatch, `tools/call` argument
//! validation and result framing. This test drives `MCPServer::serve` through
//! an in-memory [`Transport`], so what is exercised is the wire contract a
//! client actually speaks:
//!
//! ```text
//! memory_compile ──► facts + a Decision anchored to an Event fact
//!       │
//!       ├─► state_timeline   (per-dimension state intervals from those facts)
//!       ├─► decision_search  (find the recorded decision)
//!       ├─► decision_trace   (walk back to the supporting fact)
//!       └─► fact_provenance  (confidence / status / evidence / derivation)
//! ```
//!
//! Nothing is seeded by hand: the facts under test are produced by the tool
//! calls themselves, which is what made the earlier state-layer defects
//! invisible to unit tests that used hand-crafted payloads.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use mnemosyne::error::Result;
use mnemosyne::fact_store::SqliteFactStore;
use mnemosyne::mcp::memory_compile::{MemoryCompileTool, memory_compile_definition};
use mnemosyne::mcp::types::{Implementation, JSONRPCMessage};
use mnemosyne::mcp::{
    DecisionSearchTool, DecisionTraceTool, FactProvenanceTool, MCPServer, ServerBuilder,
    StateTimelineTool, Transport, decision_search_definition, decision_trace_definition,
    fact_provenance_definition, state_timeline_definition,
};
use serde_json::{Value, json};

/// An in-memory transport: replays queued JSON-RPC requests and records every
/// message the server sends back. Reaching the end of the queue is EOF, which
/// is what makes the `serve` loop terminate.
struct ScriptedTransport {
    queue: VecDeque<JSONRPCMessage>,
    sent: Vec<JSONRPCMessage>,
}

impl ScriptedTransport {
    /// Queue the given raw requests for the next `serve` run.
    fn new(requests: Vec<Value>) -> Self {
        let queue = requests
            .into_iter()
            .map(|request| {
                serde_json::from_value::<JSONRPCMessage>(request)
                    .expect("a scripted request must be a valid JSON-RPC message")
            })
            .collect();
        Self {
            queue,
            sent: Vec::new(),
        }
    }

    /// The `result` payload of the response that carries `id`.
    fn result_for(&self, id: i64) -> Value {
        self.sent
            .iter()
            .find_map(|message| match message {
                JSONRPCMessage::Response(response) if response.id == json!(id) => {
                    response.result.clone()
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("the server sent no result for request id {id}"))
    }
}

#[async_trait]
impl Transport for ScriptedTransport {
    async fn recv(&mut self) -> Result<Option<JSONRPCMessage>> {
        Ok(self.queue.pop_front())
    }

    async fn send(&mut self, msg: &JSONRPCMessage) -> Result<()> {
        self.sent.push(msg.clone());
        Ok(())
    }
}

/// Drive one real `tools/call` through the server and return the tool payload.
///
/// Panics with the server's own message when the call fails, so a broken
/// contract surfaces as a readable assertion rather than a JSON parse error.
async fn call_tool(server: &MCPServer, id: i64, tool: &str, arguments: Value) -> Value {
    let mut transport = ScriptedTransport::new(vec![json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments }
    })]);
    server
        .serve(&mut transport)
        .await
        .expect("the MCP serve loop must terminate cleanly on EOF");
    let result = transport.result_for(id);
    assert_eq!(
        result["isError"],
        json!(false),
        "tools/call `{tool}` must succeed, got {result}"
    );
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tools/call `{tool}` returned no text block: {result}"));
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tools/call `{tool}` returned a non-JSON payload ({error}): {text}")
    })
}

/// Objective: Verify the cognition layer end to end over the real MCP
/// `tools/call` path — a compiled conversation yields cognitive-state
/// dimensions, a decision anchored to a stored fact, and an auditable
/// provenance chain.
/// Invariants: `memory_compile` records exactly one decision; `state_timeline`
/// reports the goal and emotion dimensions produced by the compiler;
/// `decision_trace` resolves the decision's `because` to the commitment fact;
/// `fact_provenance` reports that fact as active with no derivation chain.
#[tokio::test]
async fn cognition_layer_full_loop_over_mcp() {
    let store = Arc::new(SqliteFactStore::open_in_memory().expect("open fact store"));
    let server = ServerBuilder::new(Implementation {
        name: "mnemosyne-e2e".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
    .tool(
        memory_compile_definition(),
        Arc::new(MemoryCompileTool::new(None, store.clone())),
    )
    .await
    .tool(
        state_timeline_definition(),
        Arc::new(StateTimelineTool::new(store.clone())),
    )
    .await
    .tool(
        fact_provenance_definition(),
        Arc::new(FactProvenanceTool::new(store.clone())),
    )
    .await
    .tool(
        decision_trace_definition(),
        Arc::new(DecisionTraceTool::new(store.clone())),
    )
    .await
    .tool(
        decision_search_definition(),
        Arc::new(DecisionSearchTool::new(store.clone())),
    )
    .await
    .build();

    // 1. Compile a conversation carrying a cognitive dimension (goal +
    //    emotion) and an explicit commitment.
    let compiled = call_tool(
        &server,
        1,
        "memory_compile",
        json!({
            "tenant_id": "tenant-a",
            "user_id": "alice",
            "messages": [
                {"role": "user", "content": "我计划学 Rust，但最近压力很大"},
                {"role": "user", "content": "我答应你明天陪你去医院"}
            ]
        }),
    )
    .await;
    let entity_id = compiled["cognition"]["user_entity_id"]
        .as_i64()
        .expect("memory_compile must report the resolved user entity id");
    assert!(entity_id > 0, "the user entity must be resolved");
    assert_eq!(
        compiled["cognition"]["decisions_recorded"],
        json!(1),
        "the promise must compile into exactly one decision"
    );
    assert!(
        compiled["cognition"]["facts_stored"]
            .as_u64()
            .unwrap_or_default()
            >= 3,
        "the goal, emotion and commitment-anchor facts must be stored, got {compiled}"
    );

    // 2. state_timeline projects the compiled facts into dimensions. This is
    //    the assertion that fails when dimension selection does not match the
    //    payload shape production facts actually carry.
    let timeline = call_tool(
        &server,
        2,
        "state_timeline",
        json!({ "entity_id": entity_id }),
    )
    .await;
    let dimensions = timeline["dimensions"]
        .as_array()
        .expect("state_timeline returns a dimensions array");
    let keys: Vec<&str> = dimensions
        .iter()
        .filter_map(|dimension| dimension["dimension"].as_str())
        .collect();
    assert!(
        keys.contains(&"goal") && keys.contains(&"emotion"),
        "the compiled goal and emotion facts must surface as dimensions, got {keys:?}"
    );
    let goal = dimensions
        .iter()
        .find(|dimension| dimension["dimension"] == "goal")
        .expect("the goal dimension is present");
    let intervals = goal["intervals"]
        .as_array()
        .expect("the goal dimension carries an intervals array");
    assert!(
        !intervals.is_empty(),
        "the goal dimension must carry at least one interval"
    );
    assert!(
        intervals[intervals.len() - 1]["to"].is_null(),
        "the latest interval must stay open (still current)"
    );

    // 3. The decision is readable and traces back to its anchoring fact.
    let search = call_tool(
        &server,
        3,
        "decision_search",
        json!({ "subject": entity_id }),
    )
    .await;
    let decisions = search["decisions"]
        .as_array()
        .expect("decision_search returns a decisions array");
    assert_eq!(decisions.len(), 1, "exactly one decision was recorded");
    assert_eq!(
        decisions[0]["status"],
        json!("open"),
        "a compiled decision is open"
    );
    assert_eq!(
        decisions[0]["object"],
        json!("我答应你明天陪你去医院"),
        "the decision carries the commitment utterance"
    );
    let decision_id = decisions[0]["decision_id"]
        .as_i64()
        .expect("a decision exposes its id");

    let trace = call_tool(
        &server,
        4,
        "decision_trace",
        json!({ "decision_id": decision_id }),
    )
    .await;
    let because = trace["because"]
        .as_array()
        .expect("decision_trace expands `because` into facts");
    assert_eq!(
        because.len(),
        1,
        "the decision must be anchored to exactly one fact"
    );
    assert_eq!(
        because[0]["content"],
        json!("我答应你明天陪你去医院"),
        "the supporting fact carries the commitment utterance"
    );
    let anchor_id = because[0]["fact_id"]
        .as_i64()
        .expect("the supporting fact exposes its id");

    // 4. fact_provenance audits the anchoring fact through the same path.
    let provenance = call_tool(
        &server,
        5,
        "fact_provenance",
        json!({ "fact_id": anchor_id }),
    )
    .await;
    assert_eq!(provenance["fact_id"], json!(anchor_id));
    assert_eq!(
        provenance["status"],
        json!("active"),
        "a freshly compiled fact is active"
    );
    assert!(
        provenance["confidence"].is_number(),
        "provenance must report a numeric confidence"
    );
    assert_eq!(
        provenance["derived_from"].as_array().map(Vec::len),
        Some(0),
        "the anchoring fact has no derivation chain"
    );
}
