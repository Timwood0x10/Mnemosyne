//! MCP tool handlers backed by the general knowledge model.
//!
//! These four tools are the frozen MCP API surface for LoreScope's
//! "Narrative World Compiler" (dev_guide §5 V2.0 冻结版):
//!
//! | Tool | Input | Output |
//! |------|-------|--------|
//! | `inspect_entity` | `{name, doc?}` | `{type, properties, events, relations, evidences}` |
//! | `timeline` | `{entity, doc?}` | `[{chapter, event, predicate, target}, ...]` |
//! | `relation_graph` | `{entity, depth?}` | `{nodes, edges}` (BFS subgraph) |
//! | `evidence` | `{query, doc?, limit?}` | `[{text, chapter, doc, confidence}, ...]` |
//!
//! Each handler holds an `Arc<SQLiteKnowledgeStore>` and delegates to the
//! high-level queries defined on [`KnowledgeStore`](crate::knowledge::KnowledgeStore).
//! Results are serialized to JSON and wrapped in a single text
//! [`ContentBlock`](super::types::ContentBlock) — matching the wire shape of
//! the existing `character_*` tools in `src/main.rs`.
//!
//! The handlers live in their own module (rather than `main.rs`) so `main.rs`
//! stays under the 1000-line rule (`plan/rules/rules.md` §1): a single
//! `register_knowledge_tools` call is all `build_server` needs.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::knowledge::SQLiteKnowledgeStore;
use crate::knowledge::store::KnowledgeStore;
use crate::mcp::ServerBuilder;
use crate::mcp::types::{ToolCallResult, ToolDefinition, ToolHandler};

// ───────────────────────────────────────────────────────────────────────────
// Shared helpers
// ───────────────────────────────────────────────────────────────────────────

/// Pull a required string argument from a tool-call `args` object.
///
/// Returns [`Error::InvalidInput`] when the key is missing or not a string,
/// so handlers can use the `?` operator and produce a structured tool error
/// rather than a panic.
fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidInput(format!("missing required argument `{key}`")))
}

/// Pull an optional string argument; `None` when the key is absent or null.
fn optional_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// Pull an optional non-negative integer argument, clamped to `[min, max]`.
///
/// Returns `default` when the key is missing or not a valid u64. Clamping
/// bounds untrusted client input (e.g. a 1_000_000 depth or a 0 limit) without
/// erroring, which matches the existing `character_network` tool's behavior.
/// `min` enforces the documented lower bound (e.g. `limit` is documented as
/// "1-200", so a client-supplied 0 is raised to 1 rather than returning an
/// empty result set).
fn optional_u64_clamped(args: &Value, key: &str, default: u64, min: u64, max: u64) -> u64 {
    args.get(key)
        .and_then(Value::as_u64)
        .unwrap_or(default)
        .clamp(min, max)
}

/// Serialize a payload to a JSON string and wrap it as a successful
/// [`ToolCallResult`]. Mirrors the `character_*` tool output shape.
fn json_result(payload: &Value) -> Result<ToolCallResult> {
    Ok(ToolCallResult::text(payload.to_string()))
}

// ───────────────────────────────────────────────────────────────────────────
// inspect_entity
// ───────────────────────────────────────────────────────────────────────────

/// Tool: inspect an entity in the narrative world model (`inspect_entity`).
///
/// Returns the object's type + properties, the events it participates in, the
/// person↔person relations touching it, the original-text evidence backing
/// those facts, and where it is mentioned. This is the Phase 0 acceptance
/// tool: `inspect_entity("赵云")` must return Object + Edges + Evidence.
struct InspectEntityTool {
    store: Arc<SQLiteKnowledgeStore>,
}

#[async_trait]
impl ToolHandler for InspectEntityTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult> {
        let name = require_str(args, "name")?;
        let doc = optional_str(args, "doc");

        let result = match self.store.inspect_entity(name, doc).await? {
            Some(r) => r,
            None => {
                // Unknown entity is a graceful tool error (not a JSON-RPC
                // error): the client asked a question, the answer is "no such
                // entity", and `is_error=true` lets an agent react.
                let ctx = doc.unwrap_or("(any document)");
                return Ok(ToolCallResult::error(format!(
                    "entity `{name}` not found in document `{ctx}`"
                )));
            }
        };
        json_result(&serde_json::to_value(&result)?)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// timeline
// ───────────────────────────────────────────────────────────────────────────

/// Tool: list an entity's timeline ordered by chapter (`timeline`).
///
/// Each row is `{chapter, event, predicate, target}`. An entity with no edges
/// yields an empty array (not an error) — an agent may legitimately ask for
/// the timeline of a freshly-created object.
struct TimelineTool {
    store: Arc<SQLiteKnowledgeStore>,
}

#[async_trait]
impl ToolHandler for TimelineTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult> {
        let entity = require_str(args, "entity")?;
        let doc = optional_str(args, "doc");

        let entries = self.store.entity_timeline(entity, doc).await?;
        json_result(&serde_json::to_value(&entries)?)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// relation_graph
// ───────────────────────────────────────────────────────────────────────────

/// Tool: BFS subgraph around an entity (`relation_graph`).
///
/// `depth` defaults to 2 and is clamped to 5 — the same bounds as the V1
/// `character_network` tool, so a client cannot request an unbounded crawl.
/// Each edge carries its `valid_from`/`valid_to` chapter bounds (dev_guide
/// §3.5: temporal is a key design decision).
struct RelationGraphTool {
    store: Arc<SQLiteKnowledgeStore>,
}

#[async_trait]
impl ToolHandler for RelationGraphTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult> {
        let entity = require_str(args, "entity")?;
        let doc = optional_str(args, "doc");
        let depth = optional_u64_clamped(args, "depth", 2, 1, 5) as usize;

        let graph = match self.store.relation_graph(entity, depth, doc).await? {
            Some(g) => g,
            None => {
                let ctx = doc.unwrap_or("(any document)");
                return Ok(ToolCallResult::error(format!(
                    "entity `{entity}` not found in document `{ctx}`"
                )));
            }
        };
        json_result(&serde_json::to_value(&graph)?)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// evidence
// ───────────────────────────────────────────────────────────────────────────

/// Tool: full-text search over original-text evidence (`evidence`).
///
/// The `query` is matched as a substring against `evidence.content` (LIKE
/// `%query%`), scoped to an optional document. `limit` defaults to 20 and is
/// clamped to 200 so a broad query cannot dump the entire evidence table.
struct EvidenceTool {
    store: Arc<SQLiteKnowledgeStore>,
}

#[async_trait]
impl ToolHandler for EvidenceTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult> {
        let query = require_str(args, "query")?;
        let doc = optional_str(args, "doc");
        let limit = optional_u64_clamped(args, "limit", 20, 1, 200) as usize;

        let hits = self.store.search_evidence(query, doc, limit).await?;
        json_result(&serde_json::to_value(&hits)?)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Registration
// ───────────────────────────────────────────────────────────────────────────

/// Register the four knowledge-model tools on a [`ServerBuilder`].
///
/// Consumes the builder and returns it chained (mirroring the
/// `builder.tool(...).await` style used in `main.rs::build_server`). Each
/// tool's `input_schema` is the frozen JSON Schema from dev_guide §5: the
/// `required` array drives the server-side argument validation in
/// [`ServerBuilder`](super::server::ServerBuilder).
pub async fn register_knowledge_tools(
    builder: ServerBuilder,
    store: Arc<SQLiteKnowledgeStore>,
) -> ServerBuilder {
    builder
        .tool(
            ToolDefinition {
                name: "inspect_entity".into(),
                description: "Inspect a narrative entity (person/event/place/...): returns \
                              {type, properties, events, relations, evidences, mentions} \
                              backed by the original text."
                    .into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Entity name (canonical or alias-resolved)."
                        },
                        "doc": {
                            "type": "string",
                            "description": "Optional document title filter (e.g. 三国演义). \
                                            Omit to match across all documents."
                        }
                    },
                    "required": ["name"]
                }),
            },
            Arc::new(InspectEntityTool {
                store: store.clone(),
            }),
        )
        .await
        .tool(
            ToolDefinition {
                name: "timeline".into(),
                description: "List an entity's timeline ordered by chapter: returns \
                              [{chapter, event, predicate, target}, ...]."
                    .into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "entity": {"type": "string", "description": "Entity name."},
                        "doc": {
                            "type": "string",
                            "description": "Optional document title filter."
                        }
                    },
                    "required": ["entity"]
                }),
            },
            Arc::new(TimelineTool {
                store: store.clone(),
            }),
        )
        .await
        .tool(
            ToolDefinition {
                name: "relation_graph".into(),
                description: "BFS subgraph around an entity up to `depth` hops: returns \
                              {nodes, edges} with temporal bounds (valid_from/valid_to)."
                    .into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "entity": {"type": "string", "description": "Starting entity name."},
                        "depth": {
                            "type": "integer",
                            "default": 2,
                            "description": "BFS depth (1-5, default 2)."
                        },
                        "doc": {
                            "type": "string",
                            "description": "Optional document title filter."
                        }
                    },
                    "required": ["entity"]
                }),
            },
            Arc::new(RelationGraphTool {
                store: store.clone(),
            }),
        )
        .await
        .tool(
            ToolDefinition {
                name: "evidence".into(),
                description: "Full-text search over original-text evidence: returns \
                              [{text, chapter, doc, confidence}, ...]."
                    .into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Substring to match against evidence content."
                        },
                        "doc": {
                            "type": "string",
                            "description": "Optional document title filter."
                        },
                        "limit": {
                            "type": "integer",
                            "default": 20,
                            "description": "Maximum hits to return (1-200, default 20)."
                        }
                    },
                    "required": ["query"]
                }),
            },
            Arc::new(EvidenceTool {
                store: store.clone(),
            }),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::store::KnowledgeStore;
    use crate::knowledge::{
        Chapter, Document, Evidence, EvidenceSourceType, KnowledgeEdge, KnowledgeObject, Mention,
        ObjectType, Origin, SQLiteKnowledgeStore,
    };
    use crate::mcp::types::JSONRPCMessage;
    use crate::mcp::{MCPServer, ServerBuilder};
    use serde_json::json;

    /// A test transport that yields pre-loaded messages and collects responses.
    struct VecTransport {
        inbox: Vec<JSONRPCMessage>,
        outbox: Vec<JSONRPCMessage>,
    }

    #[async_trait]
    impl crate::mcp::transport::Transport for VecTransport {
        async fn recv(&mut self) -> Result<Option<JSONRPCMessage>> {
            Ok(self.inbox.pop())
        }
        async fn send(&mut self, msg: &JSONRPCMessage) -> Result<()> {
            self.outbox.push(msg.clone());
            Ok(())
        }
    }

    fn now_ts() -> i64 {
        chrono::Utc::now().timestamp()
    }

    /// Seed the canonical Phase 0 acceptance fixture: 三国演义 + 赵云 + a
    /// `participated_in` event + a `trusts` relation + evidence linking both.
    ///
    /// Layout (matches `knowledge/store.rs::inspect_entity_returns_full_picture`):
    ///   - document 三国演义, chapter 41
    ///   - 赵云 (person), 刘备 (person), 单骑救主 (event)
    ///   - edge 赵云→event (participated_in, ch41)
    ///   - edge 刘备→赵云 (trusts, ch41)
    ///   - evidence "赵云怀抱阿斗，杀透重围" linked to both the object and the
    ///     participated_in edge.
    async fn seed_acceptance_fixture(store: &SQLiteKnowledgeStore) {
        let did = store
            .create_document(&Document {
                id: 0,
                title: "三国演义".into(),
                author: None,
                doc_type: Some("novel".into()),
                created_at: now_ts(),
            })
            .await
            .expect("create doc");
        let cid = store
            .create_chapter(&Chapter {
                id: 0,
                doc_id: did,
                chapter_no: 41,
                title: Some("刘玄德携民渡江".into()),
                content: "赵云怀抱阿斗，杀透重围".into(),
                start_offset: Some(0),
                end_offset: Some(14),
            })
            .await
            .expect("create chapter");
        let zhaoyun = store
            .create_object(&KnowledgeObject {
                id: 0,
                doc_id: did,
                object_type: ObjectType::Person,
                name: "赵云".into(),
                properties: json!({"aliases": ["子龙"]}),
                confidence: 0.95,
                created_at: now_ts(),
            })
            .await
            .expect("create 赵云");
        let liubei = store
            .create_object(&KnowledgeObject {
                id: 0,
                doc_id: did,
                object_type: ObjectType::Person,
                name: "刘备".into(),
                properties: json!({}),
                confidence: 1.0,
                created_at: now_ts(),
            })
            .await
            .expect("create 刘备");
        let event_id = store
            .create_object(&KnowledgeObject {
                id: 0,
                doc_id: did,
                object_type: ObjectType::Event,
                name: "单骑救主".into(),
                properties: json!({}),
                confidence: 1.0,
                created_at: now_ts(),
            })
            .await
            .expect("create event");

        // Edge: 赵云 participated_in 单骑救主 @ ch41.
        let part_edge = KnowledgeEdge {
            id: 0,
            source_id: zhaoyun,
            target_id: event_id,
            predicate: "participated_in".into(),
            properties: json!({}),
            origin: Origin::Observed,
            confidence: 1.0,
            valid_from: Some(41),
            valid_to: None,
            created_at: now_ts(),
        };
        let part_edge_id = store
            .create_edge(&part_edge)
            .await
            .expect("create part edge");

        // Edge: 刘备 trusts 赵云 @ ch41.
        let trust_edge = KnowledgeEdge {
            id: 0,
            source_id: liubei,
            target_id: zhaoyun,
            predicate: "trusts".into(),
            properties: json!({}),
            origin: Origin::Observed,
            confidence: 0.8,
            valid_from: Some(41),
            valid_to: None,
            created_at: now_ts(),
        };
        store
            .create_edge(&trust_edge)
            .await
            .expect("create trust edge");

        // Evidence: one sentence backing both the object and the edge.
        let eid = store
            .create_evidence(&Evidence {
                id: 0,
                doc_id: did,
                chapter_id: cid,
                start_offset: Some(0),
                end_offset: Some(14),
                content: "赵云怀抱阿斗，杀透重围".into(),
                created_at: now_ts(),
            })
            .await
            .expect("create evidence");
        store
            .link_evidence(EvidenceSourceType::Object, zhaoyun, eid)
            .await
            .expect("link object evidence");
        store
            .link_evidence(EvidenceSourceType::Edge, part_edge_id, eid)
            .await
            .expect("link edge evidence");

        // Mention: 赵云 appears in chapter 41.
        store
            .create_mention(&Mention {
                id: 0,
                object_id: zhaoyun,
                chapter_id: cid,
                start_offset: Some(0),
                end_offset: Some(6),
                alias_used: Some("赵云".into()),
                confidence: 1.0,
            })
            .await
            .expect("create mention");
    }

    /// Build a server with only the four knowledge tools registered.
    async fn build_server(store: Arc<SQLiteKnowledgeStore>) -> MCPServer {
        let builder = ServerBuilder::new(crate::mcp::types::Implementation {
            name: "lorescope-test".into(),
            version: "0.0.0".into(),
        });
        register_knowledge_tools(builder, store).await.build()
    }

    /// Drive a single `tools/call` against `server` and return the parsed
    /// `result` payload (the `{content, isError}` object).
    async fn call_tool(server: &MCPServer, tool: &str, args: Value) -> Value {
        let mut t = VecTransport {
            inbox: vec![JSONRPCMessage::Request(crate::mcp::types::JSONRPCRequest {
                jsonrpc: "2.0".into(),
                id: Value::from(1),
                method: "tools/call".into(),
                params: Some(json!({ "name": tool, "arguments": args })),
            })],
            outbox: vec![],
        };
        server.serve(&mut t).await.expect("serve");
        match t.outbox.pop().expect("one response") {
            JSONRPCMessage::Response(resp) => resp.result.expect("result present"),
            other => panic!("expected Response, got {other:?}"),
        }
    }

    /// Objective: Verify `tools/list` advertises all four knowledge tools with
    /// their frozen names and required-argument sets.
    /// Invariants: tools list contains inspect_entity/timeline/relation_graph/
    /// evidence; each has a non-empty description and a `required` array.
    #[tokio::test]
    async fn tools_list_advertises_four_knowledge_tools() {
        let store = Arc::new(SQLiteKnowledgeStore::open_in_memory().await.expect("open"));
        let server = build_server(store).await;

        let mut t = VecTransport {
            inbox: vec![JSONRPCMessage::Request(crate::mcp::types::JSONRPCRequest {
                jsonrpc: "2.0".into(),
                id: Value::from(1),
                method: "tools/list".into(),
                params: None,
            })],
            outbox: vec![],
        };
        server.serve(&mut t).await.expect("serve");
        let resp = match t.outbox.pop().expect("response") {
            JSONRPCMessage::Response(r) => r,
            other => panic!("expected Response, got {other:?}"),
        };
        let result = resp.result.expect("result");
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .expect("tools array");
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        for expected in &["inspect_entity", "timeline", "relation_graph", "evidence"] {
            assert!(
                names.contains(expected),
                "tools/list must advertise `{expected}`, got {names:?}"
            );
        }
        // Every tool must have a description and a required array.
        for t in tools {
            assert!(
                t.get("description")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty()),
                "tool {:?} missing description",
                t.get("name")
            );
            assert!(
                t.get("input_schema")
                    .and_then(|s| s.get("required"))
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty()),
                "tool {:?} must declare a non-empty `required` array",
                t.get("name")
            );
        }
    }

    /// Objective: Verify the Phase 0 acceptance test — `inspect_entity("赵云")`
    /// returns Object + Edges + Evidence after the fixture is seeded.
    /// Invariants: result has object.name == "赵云", >= 1 event, >= 1 relation,
    /// >= 1 evidence referencing 阿斗, >= 1 mention.
    #[tokio::test]
    async fn inspect_entity_phase0_acceptance() {
        let store = Arc::new(SQLiteKnowledgeStore::open_in_memory().await.expect("open"));
        seed_acceptance_fixture(&store).await;
        let server = build_server(store).await;

        let result = call_tool(
            &server,
            "inspect_entity",
            json!({"name": "赵云", "doc": "三国演义"}),
        )
        .await;
        assert_eq!(
            result.get("isError"),
            Some(&Value::Bool(false)),
            "must succeed"
        );

        let payload: Value = serde_json::from_str(
            result
                .get("content")
                .and_then(|c| c.as_array())
                .and_then(|a| a[0].get("text"))
                .and_then(Value::as_str)
                .expect("text"),
        )
        .expect("payload is JSON");
        assert_eq!(
            payload["object"]["name"], "赵云",
            "object name must be 赵云"
        );
        assert_eq!(payload["object"]["object_type"], "person");
        assert!(
            payload["events"].as_array().is_some_and(|a| !a.is_empty()),
            "events must be non-empty"
        );
        assert!(
            payload["relations"]
                .as_array()
                .is_some_and(|a| !a.is_empty()),
            "relations must be non-empty"
        );
        let evidences = payload["evidences"].as_array().expect("evidences array");
        assert!(!evidences.is_empty(), "evidences must be non-empty");
        assert!(
            evidences
                .iter()
                .any(|e| e["content"].as_str().is_some_and(|c| c.contains("阿斗"))),
            "at least one evidence must reference 阿斗"
        );
        assert!(
            payload["mentions"]
                .as_array()
                .is_some_and(|a| !a.is_empty()),
            "mentions must be non-empty"
        );
    }

    /// Objective: Verify `inspect_entity` returns a graceful tool error
    /// (isError=true) for an entity that does not exist.
    /// Invariants: isError=true; content text mentions the missing name.
    #[tokio::test]
    async fn inspect_entity_unknown_returns_tool_error() {
        let store = Arc::new(SQLiteKnowledgeStore::open_in_memory().await.expect("open"));
        let server = build_server(store).await;

        let result = call_tool(&server, "inspect_entity", json!({"name": "不存在的角色"})).await;
        assert_eq!(
            result.get("isError"),
            Some(&Value::Bool(true)),
            "unknown entity is a tool error"
        );
        let text = result
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|a| a[0].get("text"))
            .and_then(Value::as_str)
            .expect("error text");
        assert!(
            text.contains("不存在的角色"),
            "error must name the missing entity"
        );
    }

    /// Objective: Verify the server rejects `inspect_entity` calls that omit
    /// the required `name` argument via the input-schema validator.
    /// Invariants: error.code == -32602 (invalid params).
    #[tokio::test]
    async fn inspect_entity_missing_name_rejected() {
        let store = Arc::new(SQLiteKnowledgeStore::open_in_memory().await.expect("open"));
        let server = build_server(store).await;

        let mut t = VecTransport {
            inbox: vec![JSONRPCMessage::Request(crate::mcp::types::JSONRPCRequest {
                jsonrpc: "2.0".into(),
                id: Value::from(7),
                method: "tools/call".into(),
                params: Some(json!({"name": "inspect_entity", "arguments": {}})),
            })],
            outbox: vec![],
        };
        server.serve(&mut t).await.expect("serve");
        let resp = match t.outbox.pop().expect("response") {
            JSONRPCMessage::Response(r) => r,
            other => panic!("expected Response, got {other:?}"),
        };
        let err = resp.error.expect("must error on missing required arg");
        assert_eq!(err.code, crate::mcp::server::ERR_INVALID_PARAMS);
        assert!(err.message.contains("name"));
    }

    /// Objective: Verify `timeline` returns entries ordered by chapter and
    /// shaped as `{chapter, event, predicate, target}`.
    /// Invariants: two edges (ch1 serves, ch3 kills) yield 2 entries sorted
    /// ascending; entry[0].predicate == "serves".
    #[tokio::test]
    async fn timeline_returns_ordered_entries() {
        let store = Arc::new(SQLiteKnowledgeStore::open_in_memory().await.expect("open"));
        let did = store
            .create_document(&Document {
                id: 0,
                title: "三国演义".into(),
                author: None,
                doc_type: None,
                created_at: now_ts(),
            })
            .await
            .expect("doc");
        let lvbu = store
            .create_object(&KnowledgeObject {
                id: 0,
                doc_id: did,
                object_type: ObjectType::Person,
                name: "吕布".into(),
                properties: json!({}),
                confidence: 1.0,
                created_at: now_ts(),
            })
            .await
            .expect("lvbu");
        let dingyuan = store
            .create_object(&KnowledgeObject {
                id: 0,
                doc_id: did,
                object_type: ObjectType::Person,
                name: "丁原".into(),
                properties: json!({}),
                confidence: 1.0,
                created_at: now_ts(),
            })
            .await
            .expect("dingyuan");
        // Insert out of chapter order: kills@3 then serves@1.
        for (pred, ch) in [("kills", 3), ("serves", 1)] {
            store
                .create_edge(&KnowledgeEdge {
                    id: 0,
                    source_id: lvbu,
                    target_id: dingyuan,
                    predicate: pred.into(),
                    properties: json!({}),
                    origin: Origin::Observed,
                    confidence: 0.8,
                    valid_from: Some(ch),
                    valid_to: None,
                    created_at: now_ts(),
                })
                .await
                .expect("edge");
        }
        let server = build_server(store).await;
        let result = call_tool(
            &server,
            "timeline",
            json!({"entity": "吕布", "doc": "三国演义"}),
        )
        .await;
        let payload: Vec<Value> = serde_json::from_str(
            result
                .get("content")
                .and_then(|c| c.as_array())
                .and_then(|a| a[0].get("text"))
                .and_then(Value::as_str)
                .expect("text"),
        )
        .expect("array");
        assert_eq!(payload.len(), 2, "two edges → two entries");
        assert_eq!(payload[0]["chapter"], 1, "serves@ch1 first");
        assert_eq!(payload[0]["predicate"], "serves");
        assert_eq!(payload[1]["chapter"], 3);
        assert_eq!(payload[1]["predicate"], "kills");
        assert_eq!(payload[0]["target"], "丁原");
    }

    /// Objective: Verify `relation_graph` returns the BFS subgraph with
    /// temporal bounds on edges.
    /// Invariants: depth=1 around 刘备 (结义 关羽, 结义 张飞) yields 3 nodes
    /// and 2 edges; edges carry valid_from.
    #[tokio::test]
    async fn relation_graph_returns_temporal_subgraph() {
        let store = Arc::new(SQLiteKnowledgeStore::open_in_memory().await.expect("open"));
        let did = store
            .create_document(&Document {
                id: 0,
                title: "三国演义".into(),
                author: None,
                doc_type: None,
                created_at: now_ts(),
            })
            .await
            .expect("doc");
        let liubei = store
            .create_object(&KnowledgeObject {
                id: 0,
                doc_id: did,
                object_type: ObjectType::Person,
                name: "刘备".into(),
                properties: json!({}),
                confidence: 1.0,
                created_at: now_ts(),
            })
            .await
            .expect("liubei");
        let mut brother_ids = Vec::new();
        for name in &["关羽", "张飞"] {
            let oid = store
                .create_object(&KnowledgeObject {
                    id: 0,
                    doc_id: did,
                    object_type: ObjectType::Person,
                    name: (*name).into(),
                    properties: json!({}),
                    confidence: 1.0,
                    created_at: now_ts(),
                })
                .await
                .expect("brother");
            brother_ids.push(oid);
            store
                .create_edge(&KnowledgeEdge {
                    id: 0,
                    source_id: liubei,
                    target_id: oid,
                    predicate: "结义".into(),
                    properties: json!({}),
                    origin: Origin::Observed,
                    confidence: 1.0,
                    valid_from: Some(1),
                    valid_to: None,
                    created_at: now_ts(),
                })
                .await
                .expect("edge");
        }
        let server = build_server(store).await;
        let result = call_tool(
            &server,
            "relation_graph",
            json!({"entity": "刘备", "depth": 1}),
        )
        .await;
        let payload: Value = serde_json::from_str(
            result
                .get("content")
                .and_then(|c| c.as_array())
                .and_then(|a| a[0].get("text"))
                .and_then(Value::as_str)
                .expect("text"),
        )
        .expect("json");
        let nodes = payload["nodes"].as_array().expect("nodes");
        let edges = payload["edges"].as_array().expect("edges");
        assert_eq!(nodes.len(), 3, "root + 2 brothers");
        assert_eq!(edges.len(), 2, "two 结义 edges");
        assert!(
            edges.iter().all(|e| e["valid_from"].as_i64() == Some(1)),
            "every edge must carry valid_from=1"
        );
    }

    /// Objective: Verify `relation_graph` clamps an out-of-range `depth` to 5
    /// rather than erroring.
    /// Invariants: a depth=9999 call completes without error.
    #[tokio::test]
    async fn relation_graph_clamps_depth() {
        let store = Arc::new(SQLiteKnowledgeStore::open_in_memory().await.expect("open"));
        let did = store
            .create_document(&Document {
                id: 0,
                title: "三国演义".into(),
                author: None,
                doc_type: None,
                created_at: now_ts(),
            })
            .await
            .expect("doc");
        store
            .create_object(&KnowledgeObject {
                id: 0,
                doc_id: did,
                object_type: ObjectType::Person,
                name: "赵云".into(),
                properties: json!({}),
                confidence: 1.0,
                created_at: now_ts(),
            })
            .await
            .expect("object");
        let server = build_server(store).await;
        let result = call_tool(
            &server,
            "relation_graph",
            json!({"entity": "赵云", "depth": 9999}),
        )
        .await;
        assert_eq!(
            result.get("isError"),
            Some(&Value::Bool(false)),
            "clamped depth must succeed"
        );
    }

    /// Objective: Verify `evidence` searches across all evidence content and
    /// can be scoped to a document.
    /// Invariants: a LIKE query for "阿斗" returns 1 hit with chapter=41.
    #[tokio::test]
    async fn evidence_search_returns_hits() {
        let store = Arc::new(SQLiteKnowledgeStore::open_in_memory().await.expect("open"));
        seed_acceptance_fixture(&store).await;
        let server = build_server(store).await;

        let result = call_tool(
            &server,
            "evidence",
            json!({"query": "阿斗", "doc": "三国演义"}),
        )
        .await;
        let payload: Vec<Value> = serde_json::from_str(
            result
                .get("content")
                .and_then(|c| c.as_array())
                .and_then(|a| a[0].get("text"))
                .and_then(Value::as_str)
                .expect("text"),
        )
        .expect("array");
        assert_eq!(payload.len(), 1, "one matching evidence");
        assert_eq!(payload[0]["chapter"], 41);
        assert_eq!(payload[0]["doc"], "三国演义");
        assert_eq!(payload[0]["confidence"], 1.0);
    }

    /// Objective: Verify `evidence` clamps `limit` to 200 and accepts the
    /// default when `limit` is omitted.
    /// Invariants: both limit=99999 and limit-omitted complete without error.
    #[tokio::test]
    async fn evidence_clamps_limit() {
        let store = Arc::new(SQLiteKnowledgeStore::open_in_memory().await.expect("open"));
        seed_acceptance_fixture(&store).await;
        let server = build_server(store.clone()).await;

        let r1 = call_tool(
            &server,
            "evidence",
            json!({"query": "赵云", "limit": 99999}),
        )
        .await;
        assert_eq!(
            r1.get("isError"),
            Some(&Value::Bool(false)),
            "clamped limit must succeed"
        );

        let r2 = call_tool(&server, "evidence", json!({"query": "赵云"})).await;
        assert_eq!(
            r2.get("isError"),
            Some(&Value::Bool(false)),
            "default limit must succeed"
        );
    }

    /// Objective: Verify the helpers `require_str` and `optional_str` behave
    /// correctly for missing, null, and present values.
    /// Invariants: require_str errors on missing/null; optional_str returns None
    /// for null; both return the value when present.
    #[test]
    fn arg_helpers_round_trip() {
        let args = json!({"present": "v", "null_val": null});
        assert_eq!(require_str(&args, "present").unwrap(), "v");
        assert!(
            require_str(&args, "missing").is_err(),
            "missing key must error"
        );
        assert!(
            require_str(&args, "null_val").is_err(),
            "null value must error"
        );
        assert_eq!(optional_str(&args, "present"), Some("v"));
        assert_eq!(optional_str(&args, "missing"), None);
        assert_eq!(optional_str(&args, "null_val"), None, "null → None");
    }

    /// Objective: Verify `optional_u64_clamped` returns the default for missing
    /// keys, the value when present, clamps to `max`, and — critically — clamps
    /// values below `min` up to `min` (the `limit=0` regression: docs say
    /// "1-200" but a client-supplied 0 must not yield an empty result set).
    /// Invariants:
    /// - missing → default
    /// - in-range value (5) → 5
    /// - over-max (9999) → max
    /// - below-min (0) → min (regression guard for the documented "1-200" range)
    #[test]
    fn optional_u64_clamped_behavior() {
        let args = json!({"v": 5, "big": 9999, "zero": 0});
        assert_eq!(
            optional_u64_clamped(&args, "missing", 2, 1, 5),
            2,
            "missing → default"
        );
        assert_eq!(
            optional_u64_clamped(&args, "v", 2, 1, 5),
            5,
            "present → value"
        );
        assert_eq!(
            optional_u64_clamped(&args, "big", 2, 1, 5),
            5,
            "over-max → clamped to max"
        );
        // Regression guard: a 0 must be raised to the documented minimum (1),
        // not returned as-is. This previously returned 0 because the old
        // signature only applied `.min(max)`.
        assert_eq!(
            optional_u64_clamped(&args, "zero", 20, 1, 200),
            1,
            "below-min → clamped up to min (limit=0 regression)"
        );
    }

    /// Objective: Verify `json_result` wraps a JSON value as a successful
    /// `ToolCallResult` text block.
    /// Invariants: result has isError=false; content[0].text parses back to
    /// the original value.
    #[test]
    fn json_result_wraps_payload() {
        let payload = json!({"ok": true});
        let r = json_result(&payload).expect("wrap");
        assert!(!r.is_error);
        let text = r.content[0].text.as_deref().expect("text");
        let parsed: Value = serde_json::from_str(text).expect("parses");
        assert_eq!(parsed, payload);
    }
}
