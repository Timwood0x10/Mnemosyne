//! `memory_compile` MCP tool backed by the unified cognition compiler.
//!
//! The tool persists only immutable `Fact` records. Legacy knowledge,
//! decisions, session state, and prompts remain compatibility projections in
//! the response and are never treated as a second cognition source of truth.

use std::sync::Arc;

use serde_json::Value;

use crate::cognition_compiler::CognitionCompiler;
use crate::distiller::{Distiller, PipelineDistiller};
use crate::error::Error;
use crate::fact_store::SqliteFactStore;
use crate::mcp::types::{ToolCallResult, ToolDefinition, ToolHandler};
use crate::prompt::PromptBuilder;
use crate::types::{Experience, Memory, MemoryType, Message};

/// Handler for the unified conversation-to-cognition compilation path.
pub struct MemoryCompileTool {
    distiller: Option<Arc<PipelineDistiller>>,
    fact_store: Arc<SqliteFactStore>,
    compiler: CognitionCompiler,
}

impl MemoryCompileTool {
    /// Create the tool with the shared production stores.
    pub fn new(
        distiller: Option<Arc<PipelineDistiller>>,
        fact_store: Arc<SqliteFactStore>,
    ) -> Self {
        Self {
            distiller,
            fact_store,
            compiler: CognitionCompiler::new(),
        }
    }
}

/// Return the stable MCP schema for `memory_compile`.
pub fn memory_compile_definition() -> ToolDefinition {
    ToolDefinition {
        name: "memory_compile".into(),
        description: "Compile conversation into structured knowledge + decisions + session state. Optionally distill memories and declare what happened to earlier commitments.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "messages": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "role": {"type": "string", "enum": ["user", "assistant", "system", "tool"]},
                            "content": {"type": "string"},
                            "turn_id": {"type": "string"},
                            "tool_call_id": {"type": "string"}
                        },
                        "required": ["role", "content"]
                    }
                },
                "distill": {"type": "boolean", "default": false, "description": "Also run distillation pipeline"},
                "conversation_id": {"type": "string", "description": "Required when distill=true"},
                "tenant_id": {"type": "string", "default": "default"},
                "user_id": {"type": "string"},
                "agent_id": {"type": "string", "description": "Optional agent identity; enables compiling the agent's own promises into decisions"},
                "decision_outcomes": {
                    "type": "array",
                    "description": "Optional: declare what happened to earlier commitments, e.g. [{\"decision_id\":3,\"outcome\":\"fulfilled\"}]. Nothing is inferred from the conversation, and the FIRST outcome recorded for a decision wins: a later declaration is echoed back but never overwrites it. Unknown ids are reported as `missing` instead of failing the call.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "decision_id": {"type": "integer", "minimum": 1},
                            "outcome": {"type": "string", "enum": ["fulfilled", "violated"]}
                        },
                        "required": ["decision_id", "outcome"]
                    }
                }
            },
            "required": ["messages"]
        }),
    }
}

#[async_trait::async_trait]
impl ToolHandler for MemoryCompileTool {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let messages_raw = args
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::InvalidInput("missing `messages` array".into()))?;
        let messages = parse_messages(messages_raw)?;
        // Decision closure: the host declares what happened to earlier
        // commitments. This is the ONLY outcome write path — the decision MCP
        // surface is read-only, and nothing is inferred from the conversation,
        // so a promise is never closed by a guess. Declarations are validated
        // before the first write, so a malformed one cannot leave a
        // half-applied call behind.
        let outcome_reports = self.record_decision_outcomes(args)?;
        let tenant_id = args
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let user_id = args.get("user_id").and_then(Value::as_str).unwrap_or("");

        let user_entity_id = self.fact_store.resolve_user(tenant_id, user_id)?;
        let logical_time = chrono::Utc::now().timestamp() as i32;
        let compiled =
            self.compiler
                .compile_conversation(tenant_id, &messages, user_entity_id, logical_time);

        // Decision write path: explicit commitments become first-class
        // `Decision` rows so `decision_trace` can walk from a decision back to
        // the facts that support it. The frozen plan keeps the decision MCP
        // surface read-only, so this compile step IS the write path.
        //
        // A commitment is itself experience worth keeping: the utterance is
        // stored as an Event fact and the decision points at it, so every
        // decision stays anchored to a stored fact (promise markers are not in
        // the observation marker tables, so nothing else anchors it).
        //
        // Everything is prepared first and committed in ONE transaction: a
        // decision rejected by validation, or a failed agent lookup, must not
        // leave this conversation's facts behind for a retry to duplicate.
        let commitments =
            self.compiled_commitments(args, tenant_id, &messages, user_entity_id, logical_time)?;
        let (stored_facts, decisions_recorded) = self
            .fact_store
            .insert_compilation(&compiled.facts, &commitments)?;

        let builder = PromptBuilder;
        let recent_count = messages.len().min(6);
        let prompt = builder.build(
            &messages[messages.len() - recent_count..],
            &compiled.compatibility,
        );
        let memories = self
            .distill_if_requested(args, tenant_id, user_id, &messages, &compiled.compatibility)
            .await?;

        // Persist compiled knowledge into the memories table (not just the
        // response projection): a conversation that yields knowledge must
        // survive in the store, otherwise it only lives in the JSON reply and
        // is lost on the next run. Runs regardless of the `distill` flag.
        if let Some(distiller) = &self.distiller {
            persist_compatible_knowledge(distiller, tenant_id, &compiled.compatibility.knowledge)
                .await?;
        }

        let payload = serde_json::json!({
            "knowledge": compiled.compatibility.knowledge,
            "decisions": compiled.compatibility.decisions,
            "session": compiled.compatibility.session,
            "prompt": prompt,
            "cognition": {
                "user_entity_id": user_entity_id,
                "tenant_id": tenant_id,
                "user_id": if user_id.is_empty() { "default" } else { user_id },
                "observations_compiled": compiled.observations.len(),
                "facts_compiled": compiled.facts.len(),
                "facts_stored": stored_facts,
                "decisions_recorded": decisions_recorded,
                "decision_outcomes": outcome_reports,
                // Lexicon provenance: which lexicon version produced these facts
                // (ELITE_LEXICON_PLAN §15 — hash in compile diagnostics).
                "lexicon": {
                    "content_hash": crate::lexicon::global().content_hash(),
                    "lexemes": crate::lexicon::global().lexemes().len(),
                },
            },
            "distilled_memories": memories,
        });
        Ok(ToolCallResult::text(payload.to_string()))
    }
}

impl MemoryCompileTool {
    /// Record the outcomes the caller declared for earlier decisions.
    ///
    /// Every declaration is validated before the first write, so a malformed
    /// entry aborts the call without touching any decision. The report echoes
    /// the **resulting** state of each decision: an id that does not exist comes
    /// back as `missing`, and a declaration that lost to an already-recorded
    /// outcome comes back carrying the original one — the caller always sees
    /// what the store actually holds instead of a silent success.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when a declaration is malformed, and a
    /// storage error when the update fails.
    fn record_decision_outcomes(&self, args: &Value) -> Result<Vec<Value>, Error> {
        let Some(declared) = args.get("decision_outcomes") else {
            return Ok(Vec::new());
        };
        if declared.is_null() {
            return Ok(Vec::new());
        }
        let declared = declared
            .as_array()
            .ok_or_else(|| Error::InvalidInput("`decision_outcomes` must be an array".into()))?;

        let mut validated = Vec::with_capacity(declared.len());
        for entry in declared {
            let decision_id = entry
                .get("decision_id")
                .and_then(Value::as_i64)
                .ok_or_else(|| {
                    Error::InvalidInput(
                        "each `decision_outcomes` entry needs an integer `decision_id`".into(),
                    )
                })?;
            let raw = entry
                .get("outcome")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    Error::InvalidInput(
                        "each `decision_outcomes` entry needs a string `outcome`".into(),
                    )
                })?;
            let outcome = crate::decision::DecisionOutcome::parse(raw).ok_or_else(|| {
                Error::InvalidInput(format!(
                    "unknown outcome `{raw}`, expected `fulfilled` or `violated`"
                ))
            })?;
            validated.push((decision_id, outcome));
        }

        let mut reports = Vec::with_capacity(validated.len());
        for (decision_id, outcome) in validated {
            reports.push(
                match self.fact_store.set_decision_outcome(decision_id, outcome)? {
                    Some(decision) => serde_json::json!({
                        "decision_id": decision.id,
                        "outcome": decision.outcome.map(crate::decision::DecisionOutcome::as_str),
                        "status": decision.status.as_str(),
                    }),
                    None => serde_json::json!({ "decision_id": decision_id, "missing": true }),
                },
            );
        }
        Ok(reports)
    }

    /// Extract this conversation's commitments together with the fact that
    /// anchors each one, without writing anything.
    ///
    /// Resolving the agent entity happens here (it may create the entity row),
    /// but every fact and decision is only *prepared*: the store writes them in
    /// one transaction afterwards, so nothing half-compiled can be left behind.
    ///
    /// # Errors
    ///
    /// Returns an error when the agent identity cannot be resolved.
    fn compiled_commitments(
        &self,
        args: &Value,
        tenant_id: &str,
        messages: &[Message],
        user_entity_id: i64,
        logical_time: i32,
    ) -> Result<Vec<(crate::cognition::Fact, crate::decision::Decision)>, Error> {
        let mut commitments = Vec::new();
        for (role, subject) in self.commitment_speakers(args, tenant_id, user_entity_id)? {
            for decision in
                crate::commitment::commitments_from_messages(messages, role, subject, logical_time)
            {
                commitments.push((
                    crate::commitment::anchor_fact(&decision, logical_time),
                    decision,
                ));
            }
        }
        Ok(commitments)
    }

    /// Resolve which `(role, entity)` speaker channels to scan for commitments.
    ///
    /// The user channel always exists. The agent channel is only used when the
    /// caller supplied an `agent_id`, so a compile without one never
    /// materialises an agent entity as a side effect.
    fn commitment_speakers(
        &self,
        args: &Value,
        tenant_id: &str,
        user_entity_id: i64,
    ) -> Result<Vec<(&'static str, i64)>, Error> {
        let mut speakers = vec![("user", user_entity_id)];
        if let Some(agent_id) = args.get("agent_id").and_then(Value::as_str) {
            if !agent_id.is_empty() {
                speakers.push((
                    "assistant",
                    self.fact_store.resolve_agent(tenant_id, agent_id)?,
                ));
            }
        }
        Ok(speakers)
    }

    async fn distill_if_requested(
        &self,
        args: &Value,
        tenant_id: &str,
        user_id: &str,
        messages: &[Message],
        compatibility: &crate::types::CompiledConversation,
    ) -> Result<Option<Vec<Memory>>, Error> {
        let should_distill = args
            .get("distill")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !should_distill {
            return Ok(None);
        }
        let Some(distiller) = &self.distiller else {
            return Ok(None);
        };

        let conversation_id = args
            .get("conversation_id")
            .and_then(Value::as_str)
            .unwrap_or("compile");
        let memories = distiller
            .distill(conversation_id, messages, tenant_id, user_id)
            .await?;
        persist_compatible_decisions(distiller, tenant_id, &compatibility.decisions).await?;
        Ok(Some(memories))
    }
}

async fn persist_compatible_decisions(
    distiller: &PipelineDistiller,
    tenant_id: &str,
    decisions: &[crate::types::Decision],
) -> Result<(), Error> {
    let noise_filter = crate::filter::NoiseFilter::new();
    let security_filter = crate::filter::SecurityFilter::new();
    let existing = distiller
        .store()
        .get_by_memory_type(tenant_id, MemoryType::Knowledge)
        .await?;

    for decision in decisions {
        let content = format!(
            "Decision: {} — Rationale: {}",
            decision.decision, decision.rationale
        );
        let probe = Message::new("user", &content);
        if security_filter.is_sensitive(&probe)
            || noise_filter.is_noise(&probe)
            || existing
                .iter()
                .any(|experience| experience.content == content)
        {
            continue;
        }
        let mut experience = Experience::new(
            tenant_id,
            MemoryType::Knowledge,
            content,
            decision.importance,
        );
        experience.source = "compile".to_string();
        distiller.store().create(&experience).await?;
    }
    Ok(())
}

/// Persist compiled knowledge (`CompiledConversation.knowledge`) into the
/// memories table. Previously this list only appeared in the JSON response
/// projection and was never stored, so a compiled conversation's knowledge
/// vanished on the next run. Filters noise/secrets and dedupes against
/// existing Knowledge rows, mirroring [`persist_compatible_decisions`].
async fn persist_compatible_knowledge(
    distiller: &PipelineDistiller,
    tenant_id: &str,
    knowledge: &[crate::types::Memory],
) -> Result<(), Error> {
    let noise_filter = crate::filter::NoiseFilter::new();
    let security_filter = crate::filter::SecurityFilter::new();
    let existing = distiller
        .store()
        .get_by_memory_type(tenant_id, MemoryType::Knowledge)
        .await?;

    for memory in knowledge {
        let content = memory.content.trim();
        if content.is_empty() {
            continue;
        }
        let probe = Message::new("user", content);
        if security_filter.is_sensitive(&probe)
            || noise_filter.is_noise(&probe)
            || existing
                .iter()
                .any(|experience| experience.content == content)
        {
            continue;
        }
        let mut experience =
            Experience::new(tenant_id, MemoryType::Knowledge, content, memory.importance);
        experience.source = "compile".to_string();
        distiller.store().create(&experience).await?;
    }
    Ok(())
}

fn parse_messages(values: &[Value]) -> Result<Vec<Message>, Error> {
    values
        .iter()
        .map(|raw| {
            let object = raw
                .as_object()
                .ok_or_else(|| Error::InvalidInput("each message must be an object".into()))?;
            let role = object
                .get("role")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::InvalidInput("message missing `role`".into()))?;
            let content = object
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::InvalidInput("message missing `content`".into()))?;
            let mut message = Message::new(role, content);
            if let Some(turn_id) = object.get("turn_id").and_then(Value::as_str) {
                message.turn_id = Some(turn_id.to_string());
            }
            if let Some(tool_call_id) = object.get("tool_call_id").and_then(Value::as_str) {
                message.tool_call_id = Some(tool_call_id.to_string());
            }
            Ok(message)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cognition::FactStore;

    fn result_payload(result: &ToolCallResult) -> Value {
        let text = result
            .content
            .first()
            .and_then(|block| block.text.as_deref())
            .expect("A successful compile result must contain one text payload");
        serde_json::from_str(text).expect("The compile text payload must be valid JSON")
    }

    /// Objective: Verify the MCP compatibility contract after moving the handler.
    /// Invariants: Legacy fields remain and cognition facts use the resolved user identity.
    #[tokio::test]
    async fn handler_preserves_legacy_fields_and_persists_cognition() {
        let fact_store = Arc::new(
            SqliteFactStore::open_in_memory()
                .expect("An isolated fact store must initialize for the handler test"),
        );
        let tool = MemoryCompileTool::new(None, fact_store.clone());
        let result = tool
            .call(&serde_json::json!({
                "tenant_id": "tenant-a",
                "user_id": "alice",
                "messages": [
                    {"role": "user", "content": "I want to learn Rust."},
                    {"role": "assistant", "content": "Let us start."}
                ]
            }))
            .await
            .expect("The unified memory_compile handler should accept valid messages");
        let payload = result_payload(&result);

        for field in [
            "knowledge",
            "decisions",
            "session",
            "prompt",
            "cognition",
            "distilled_memories",
        ] {
            assert!(
                payload.get(field).is_some(),
                "The compatibility response must retain the `{field}` field"
            );
        }
        let entity_id = payload["cognition"]["user_entity_id"]
            .as_i64()
            .expect("The cognition response must expose a numeric user entity id");
        let facts = fact_store
            .get_facts(entity_id)
            .expect("Facts emitted by memory_compile must remain readable");
        assert_eq!(
            payload["cognition"]["facts_stored"].as_u64(),
            Some(facts.len() as u64),
            "The reported stored count must match the committed fact rows"
        );
        assert!(
            !facts.is_empty(),
            "A supported user goal marker must persist at least one immutable fact"
        );
    }

    /// Objective: Verify optional message identifiers survive MCP parsing.
    /// Invariants: Turn and tool-call identifiers remain attached to parsed messages.
    #[test]
    fn parser_preserves_optional_message_identifiers() {
        let messages = parse_messages(&[serde_json::json!({
            "role": "tool",
            "content": "completed",
            "turn_id": "turn-7",
            "tool_call_id": "call-9"
        })])
        .expect("A tool result with optional identifiers should parse");

        assert_eq!(
            messages[0].turn_id.as_deref(),
            Some("turn-7"),
            "The parser must preserve the logical turn identifier"
        );
        assert_eq!(
            messages[0].tool_call_id.as_deref(),
            Some("call-9"),
            "The parser must preserve the tool call identifier"
        );
    }

    /// Objective: Verify malformed message objects fail with typed input errors.
    /// Invariants: Invalid requests do not create a default user or persist facts.
    #[tokio::test]
    async fn handler_rejects_message_without_content() {
        let fact_store = Arc::new(
            SqliteFactStore::open_in_memory()
                .expect("An isolated fact store must initialize for invalid-input testing"),
        );
        let tool = MemoryCompileTool::new(None, fact_store.clone());
        let error = tool
            .call(&serde_json::json!({"messages": [{"role": "user"}]}))
            .await
            .expect_err("A message without content must fail before identity resolution");

        assert!(
            matches!(error, Error::InvalidInput(ref message) if message.contains("content")),
            "The missing content error must remain a typed InvalidInput, got {error:?}"
        );
        assert!(
            fact_store
                .find_entity("default", Some("default"), "User")
                .expect("Entity lookup after invalid input must succeed")
                .is_none(),
            "Invalid input must not create the default user as a side effect"
        );
    }

    /// Objective: Verify the decision write path end to end — a compiled
    /// conversation containing an explicit promise must persist a `Decision`
    /// that points back at the facts compiled from the same utterance, so
    /// `decision_trace` can walk from a decision to its evidence.
    /// Invariants: exactly one decision is recorded for the user entity, it is
    /// open with no outcome, and its supporting-fact list is non-empty.
    #[tokio::test]
    async fn compile_records_commitments_as_decisions() {
        let fact_store = Arc::new(
            SqliteFactStore::open_in_memory()
                .expect("An isolated fact store must initialize for the handler test"),
        );
        let tool = MemoryCompileTool::new(None, fact_store.clone());
        let result = tool
            .call(&serde_json::json!({
                "tenant_id": "tenant-a",
                "user_id": "alice",
                "messages": [
                    {"role": "user", "content": "我答应你明天陪你去医院"}
                ]
            }))
            .await
            .expect("A commitment conversation must compile");
        let payload = result_payload(&result);

        assert_eq!(
            payload["cognition"]["decisions_recorded"],
            serde_json::json!(1),
            "the promise must be recorded as one decision"
        );

        let user_entity_id = payload["cognition"]["user_entity_id"]
            .as_i64()
            .expect("the cognition payload exposes the user entity id");
        let decisions = fact_store
            .get_decisions(user_entity_id)
            .expect("decisions are readable after the compile");
        assert_eq!(decisions.len(), 1, "exactly one decision is persisted");
        assert_eq!(decisions[0].verb, "promise", "答应 compiles to a promise");
        assert_eq!(decisions[0].object, "我答应你明天陪你去医院");
        assert_eq!(
            decisions[0].status,
            crate::decision::DecisionStatus::Open,
            "a freshly compiled decision is open"
        );
        assert!(
            decisions[0].outcome.is_none(),
            "a freshly compiled decision has no outcome"
        );

        let subject_facts = fact_store
            .get_facts(user_entity_id)
            .expect("compiled facts are readable");
        assert_eq!(
            decisions[0].because.len(),
            1,
            "the decision must point at exactly the fact anchored to its utterance"
        );
        let anchor_id = decisions[0].because[0];
        let anchor = subject_facts
            .iter()
            .find(|fact| fact.id == Some(anchor_id))
            .expect("the anchor fact must be stored alongside the decision");
        assert_eq!(
            anchor.fact_type,
            crate::cognition::FactType::Event,
            "a commitment is anchored as an Event fact"
        );
        assert_eq!(
            anchor.payload["content"], "我答应你明天陪你去医院",
            "the anchor fact carries the commitment utterance verbatim"
        );
        assert_eq!(
            payload["cognition"]["facts_stored"].as_u64(),
            Some(subject_facts.len() as u64),
            "the reported stored count must include the anchor fact"
        );
    }

    /// Objective: Verify the decision loop can actually be CLOSED: the caller
    /// declares what happened to an earlier commitment, the store records it
    /// exactly once, and the report echoes the state the store really holds.
    /// Before this path existed a decision stayed `open` forever, because no
    /// production code ever wrote `outcome`.
    /// Invariants: `fulfilled` closes the decision; a later `violated` is echoed
    /// back as `fulfilled` and does NOT overwrite it; an unknown id is reported
    /// as `missing` instead of failing the call; a malformed declaration is
    /// rejected BEFORE anything is written.
    #[tokio::test]
    async fn declared_outcomes_close_a_decision_exactly_once() {
        let fact_store = Arc::new(
            SqliteFactStore::open_in_memory()
                .expect("An isolated fact store must initialize for the handler test"),
        );
        let tool = MemoryCompileTool::new(None, fact_store.clone());
        let promise = serde_json::json!({"role": "user", "content": "我答应你明天陪你去医院"});

        let compiled = tool
            .call(&serde_json::json!({
                "tenant_id": "tenant-a",
                "user_id": "alice",
                "messages": [promise]
            }))
            .await
            .expect("the commitment must compile");
        let subject = result_payload(&compiled)["cognition"]["user_entity_id"]
            .as_i64()
            .expect("the compile reports the user entity id");
        let decision_id = fact_store
            .get_decisions(subject)
            .expect("decisions are readable")
            .first()
            .and_then(|decision| decision.id)
            .expect("the promise is stored with an id");

        // 1. A declared outcome closes the decision.
        let closed = tool
            .call(&serde_json::json!({
                "tenant_id": "tenant-a",
                "user_id": "alice",
                "messages": [{"role": "user", "content": "今天天气不错"}],
                "decision_outcomes": [{"decision_id": decision_id, "outcome": "fulfilled"}]
            }))
            .await
            .expect("a declared outcome must be recorded");
        let reports = result_payload(&closed)["cognition"]["decision_outcomes"].clone();
        assert_eq!(
            reports[0]["outcome"],
            serde_json::json!("fulfilled"),
            "the report must echo the recorded outcome, got {reports}"
        );
        assert_eq!(
            reports[0]["status"],
            serde_json::json!("closed"),
            "recording an outcome closes the decision, got {reports}"
        );
        assert_eq!(
            fact_store
                .get_decision(decision_id)
                .expect("read the decision")
                .expect("the decision exists")
                .outcome,
            Some(crate::decision::DecisionOutcome::Fulfilled),
            "the outcome must be persisted"
        );

        // 2. The first outcome wins — the caller sees the truth, not its own
        //    declaration echoed back.
        let conflicting = tool
            .call(&serde_json::json!({
                "tenant_id": "tenant-a",
                "user_id": "alice",
                "messages": [{"role": "user", "content": "今天天气不错"}],
                "decision_outcomes": [{"decision_id": decision_id, "outcome": "violated"}]
            }))
            .await
            .expect("a conflicting declaration is not an error");
        let reports = result_payload(&conflicting)["cognition"]["decision_outcomes"].clone();
        assert_eq!(
            reports[0]["outcome"],
            serde_json::json!("fulfilled"),
            "an already-recorded outcome must never be overwritten, got {reports}"
        );

        // 3. An unknown id is reported, not fatal.
        let unknown = tool
            .call(&serde_json::json!({
                "tenant_id": "tenant-a",
                "user_id": "alice",
                "messages": [{"role": "user", "content": "今天天气不错"}],
                "decision_outcomes": [{"decision_id": decision_id + 999, "outcome": "violated"}]
            }))
            .await
            .expect("an unknown decision id must not fail the call");
        assert_eq!(
            result_payload(&unknown)["cognition"]["decision_outcomes"][0]["missing"],
            serde_json::json!(true),
            "an unknown id must be reported as missing"
        );

        // 4. Malformed input aborts before the first write.
        let error = tool
            .call(&serde_json::json!({
                "tenant_id": "tenant-b",
                "user_id": "bob",
                "messages": [{"role": "user", "content": "我答应你明天陪你去医院"}],
                "decision_outcomes": [{"decision_id": decision_id, "outcome": "maybe"}]
            }))
            .await
            .expect_err("an unknown outcome value must be rejected");
        assert!(
            matches!(error, Error::InvalidInput(_)),
            "an unknown outcome is InvalidInput, got {error:?}"
        );
        let bob = fact_store
            .resolve_user("tenant-b", "bob")
            .expect("resolve the second user");
        assert!(
            fact_store.get_facts(bob).expect("read facts").is_empty(),
            "a rejected call must not store any fact"
        );
        assert!(
            fact_store
                .get_decisions(bob)
                .expect("read decisions")
                .is_empty(),
            "a rejected call must not store any decision"
        );
    }
}
