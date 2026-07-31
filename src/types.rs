//! Core domain types for the memory distillation pipeline.
//!
//! These DTOs mirror the source project's `api/experience/types.go` and
//! `api/experience/repository.go`, translated to idiomatic Rust.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Type of memory, used for classification and retrieval filtering.
///
/// - `Knowledge`: factual problem-solution pairs (long-lived).
/// - `Preference`: user preferences (e.g. "likes tab indentation").
/// - `Skill`: reusable procedural know-how (long-lived).
/// - `Experience`: situational lessons drawn from a session (medium TTL).
/// - `Interaction`: transient interaction patterns (short TTL).
/// - `Profile`: stable user profile facts (e.g. "works in Rust").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    Knowledge,
    Preference,
    Skill,
    Experience,
    Interaction,
    Profile,
}

impl MemoryType {
    /// Default per-type TTL in seconds.
    ///
    /// `Knowledge`, `Skill`, and `Profile` are long-lived (30 days),
    /// `Experience` medium (14 days), `Preference` (7 days),
    /// `Interaction` short (24 hours).
    pub fn default_ttl_seconds(self) -> i64 {
        match self {
            MemoryType::Knowledge | MemoryType::Skill | MemoryType::Profile => 30 * 24 * 3600,
            MemoryType::Experience => 14 * 24 * 3600,
            MemoryType::Preference => 7 * 24 * 3600,
            MemoryType::Interaction => 24 * 3600,
        }
    }

    /// String identifier used in storage and MCP JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryType::Knowledge => "knowledge",
            MemoryType::Preference => "preference",
            MemoryType::Skill => "skill",
            MemoryType::Experience => "experience",
            MemoryType::Interaction => "interaction",
            MemoryType::Profile => "profile",
        }
    }
}

impl std::fmt::Display for MemoryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for MemoryType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "knowledge" => Ok(MemoryType::Knowledge),
            "preference" => Ok(MemoryType::Preference),
            "skill" => Ok(MemoryType::Skill),
            "experience" => Ok(MemoryType::Experience),
            "interaction" => Ok(MemoryType::Interaction),
            "profile" => Ok(MemoryType::Profile),
            other => Err(format!("unknown memory type: {other}")),
        }
    }
}

/// How an experience was extracted from a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExtractionMethod {
    /// Direct `user -> assistant` extraction.
    Direct,
    /// Cross-turn: `user -> assistant -> user -> assistant`.
    CrossTurn,
}

impl ExtractionMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            ExtractionMethod::Direct => "direct",
            ExtractionMethod::CrossTurn => "cross-turn",
        }
    }
}

/// A conversation message, the atomic input to the distiller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Speaker role: `user`, `assistant`, or `system`.
    pub role: String,
    /// Raw message content.
    pub content: String,
    /// Optional tool call identifier that produced this message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// If this message is a tool invocation (role=assistant), the tool details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_invocation: Option<ToolInvocation>,
    /// Optional logical turn identifier for grouping messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

impl Message {
    /// Build a new message with the given role and content.
    #[must_use]
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tool_call_id: None,
            tool_invocation: None,
            turn_id: None,
        }
    }

    /// Returns `true` if the speaker is the `user`.
    #[must_use]
    pub fn is_user(&self) -> bool {
        self.role == "user"
    }

    /// Returns `true` if the speaker is the `assistant`.
    #[must_use]
    pub fn is_assistant(&self) -> bool {
        self.role == "assistant"
    }
}

/// A persisted experience record, the storage-level unit.
///
/// `Experience` corresponds to a single distilled memory in the vector store;
/// it stores both the structured content (`problem` + `solution`) and the
/// embedding vector used for similarity search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Experience {
    /// Unique record identifier (UUIDv4).
    pub id: String,
    /// Tenant identifier for multi-tenant isolation.
    pub tenant_id: String,
    /// Optional user identifier.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user_id: String,
    /// Memory classification type.
    pub memory_type: MemoryType,
    /// The distilled problem statement (short).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub problem: String,
    /// The distilled solution / answer.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub solution: String,
    /// Combined human-readable content of the memory.
    pub content: String,
    /// Confidence / importance score in `[0.0, 1.0]`.
    pub confidence: f64,
    /// Origin source identifier (e.g. conversation id).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// Embedding vector for similarity search.
    ///
    /// Empty when the record is read from storage without a vector join;
    /// callers that need the vector should request it explicitly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vector: Vec<f32>,
    /// How this experience was extracted.
    pub extraction_method: ExtractionMethod,
    /// Creation timestamp (UTC).
    pub created_at: DateTime<Utc>,
    /// Expiry timestamp (UTC); empty string semantics = never expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Free-form metadata bag (JSON object in storage).
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub metadata: Metadata,
    /// Cosine distance reported by sqlite-vec for vector searches.
    ///
    /// Populated only by [`crate::store::SQLiteVecStore::search_by_vector`];
    /// zero otherwise. Convert to similarity with `1.0 - distance`.
    #[serde(default)]
    pub distance: f64,
}

impl Experience {
    /// Build a new experience with a fresh UUID and current timestamp.
    #[must_use]
    pub fn new(
        tenant_id: impl Into<String>,
        memory_type: MemoryType,
        content: impl Into<String>,
        confidence: f64,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            tenant_id: tenant_id.into(),
            user_id: String::new(),
            memory_type,
            problem: String::new(),
            solution: String::new(),
            content: content.into(),
            confidence: confidence.clamp(0.0, 1.0),
            source: String::new(),
            vector: Vec::new(),
            extraction_method: ExtractionMethod::Direct,
            created_at: Utc::now(),
            expires_at: None,
            metadata: Metadata::default(),
            distance: 0.0,
        }
    }
}

/// Origin of a memory, used for traceability and Evolution feedback.
///
/// - `Distillation`: produced automatically by the pipeline from a conversation.
/// - `Manual`: written explicitly by an agent via the `memory_store` tool.
/// - `Feedback`: derived from agent usage feedback (promotion/merge).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemorySource {
    Distillation,
    Manual,
    Feedback,
}

impl MemorySource {
    /// String identifier used in storage and MCP JSON.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            MemorySource::Distillation => "distillation",
            MemorySource::Manual => "manual",
            MemorySource::Feedback => "feedback",
        }
    }
}

impl std::fmt::Display for MemorySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for MemorySource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "distillation" => Ok(MemorySource::Distillation),
            "manual" => Ok(MemorySource::Manual),
            "feedback" => Ok(MemorySource::Feedback),
            other => Err(format!("unknown memory source: {other}")),
        }
    }
}

/// A working memory candidate during distillation.
///
/// `Memory` is the intermediate form produced by the classifier/scorer and
/// consumed by the resolver and store; it carries the full lifecycle metadata
/// including TTL and expiry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    /// Unique memory identifier.
    pub id: String,
    /// Tenant identifier.
    pub tenant_id: String,
    /// Optional user identifier.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user_id: String,
    /// Memory classification type.
    pub memory_type: MemoryType,
    /// Human-readable content (full text).
    pub content: String,
    /// Short compressed summary (Phase 5 output). Empty when no compression ran.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// Importance score in `[0.0, 1.0]`.
    pub importance: f64,
    /// Origin category of this memory.
    #[serde(default = "default_memory_source")]
    pub source_type: MemorySource,
    /// Origin source identifier (e.g. conversation id).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// Optional embedding vector.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vector: Vec<f32>,
    /// Time-to-live duration.
    pub ttl: Duration,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Expiry timestamp (`created_at + ttl`).
    pub expires_at: DateTime<Utc>,
    /// Free-form metadata.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub metadata: Metadata,
}

/// Default value for `MemorySource` used by serde.
#[must_use]
fn default_memory_source() -> MemorySource {
    MemorySource::Distillation
}

impl Memory {
    /// Build a new memory with a fresh UUID, current timestamp, and the
    /// type's default TTL applied to `expires_at`.
    #[must_use]
    pub fn new(
        tenant_id: impl Into<String>,
        memory_type: MemoryType,
        content: impl Into<String>,
        importance: f64,
    ) -> Self {
        let created_at = Utc::now();
        let ttl = Duration::seconds(memory_type.default_ttl_seconds());
        let expires_at = created_at + ttl;
        Self {
            id: Uuid::new_v4().to_string(),
            tenant_id: tenant_id.into(),
            user_id: String::new(),
            memory_type,
            content: content.into(),
            summary: String::new(),
            importance: importance.clamp(0.0, 1.0),
            source_type: MemorySource::Distillation,
            source: String::new(),
            vector: Vec::new(),
            ttl,
            created_at,
            expires_at,
            metadata: Metadata::default(),
        }
    }

    /// Returns `true` if this memory has expired relative to `now`.
    #[must_use]
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }

    /// Returns the display text used for retrieval and ranking.
    ///
    /// Prefers the compressed `summary` when present, otherwise falls back
    /// to the full `content`. This keeps the retrieval pipeline agnostic to
    /// whether Phase 5 compression ran.
    #[must_use]
    pub fn display_text(&self) -> &str {
        if self.summary.is_empty() {
            &self.content
        } else {
            &self.summary
        }
    }
}

/// JSON-serializable metadata bag.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    /// Inner ordered map (preserved by serde_json).
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub entries: serde_json::Map<String, serde_json::Value>,
}

impl Metadata {
    /// Returns `true` if the metadata bag contains no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Insert a key-value pair, returning the previous value if any.
    pub fn insert(
        &mut self,
        key: impl Into<String>,
        value: serde_json::Value,
    ) -> Option<serde_json::Value> {
        self.entries.insert(key.into(), value)
    }

    /// Returns `true` if the metadata bag contains the given key.
    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Get a value by key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.entries.get(key)
    }
}

/// A decision extracted from conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    /// What was decided (summary).
    pub decision: String,
    /// Rationale.
    pub rationale: String,
    /// The module/area this decision applies to.
    pub module: String,
    /// Importance.
    pub importance: f64,
}

/// A tool invocation attached to a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvocation {
    /// Tool name (e.g. `web_search`, `read_file`).
    pub name: String,
    /// Arguments as JSON string.
    pub arguments: String,
}

/// A single step in the LLM's reasoning chain via tool use.
///
/// This captures the *why → what → how* arc, not just I/O:
/// 1. What user need triggered this step (`trigger`)
/// 2. Which tool was selected (`tool_name` + `tool_args`)
/// 3. Whether the call succeeded or failed (`status`)
/// 4. How the LLM reasoned about the result (`reasoning`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningStep {
    /// The user message (or context) that triggered this tool use — full, undistorted.
    pub trigger: String,
    /// Tool name invoked.
    pub tool_name: String,
    /// Arguments passed to the tool — full JSON, undistorted.
    pub tool_args: String,
    /// Execution outcome: `"ok"`, `"error"`, or `"timeout"`.
    pub status: String,
    /// How the LLM processed the tool output to form its response — full, undistorted.
    pub reasoning: String,
}

/// Short-term working state — not persisted as long-term memory.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    /// Current active goal.
    pub current_goal: String,
    /// Module being worked on.
    pub current_module: String,
    /// Files being edited.
    pub current_files: Vec<String>,
    /// Open/unresolved problems.
    pub open_problems: Vec<String>,
    /// Pending tasks.
    pub todo: Vec<String>,
    /// Recent decisions made this session.
    pub recent_decisions: Vec<String>,
    /// Recent reasoning chain steps (up to 5).
    pub reasoning_chain: Vec<ReasoningStep>,
}

/// Output of the conversation compiler.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompiledConversation {
    /// Long-term knowledge facts.
    pub knowledge: Vec<Memory>,
    /// Decisions made.
    pub decisions: Vec<Decision>,
    /// Session working state.
    pub session: SessionState,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// Objective: Verify MemoryType string round-trips correctly.
    /// Invariants: as_str -> FromStr must be identity for all variants.
    #[test]
    fn memory_type_round_trip() {
        for variant in [
            MemoryType::Knowledge,
            MemoryType::Preference,
            MemoryType::Skill,
            MemoryType::Experience,
            MemoryType::Interaction,
            MemoryType::Profile,
        ] {
            let s = variant.as_str();
            let back = MemoryType::from_str(s).expect("round trip");
            assert_eq!(variant, back, "variant {variant:?} should round-trip");
        }
    }

    /// Objective: Verify that an unknown memory type string produces a clear error.
    /// Invariants: FromStr error path returns descriptive message.
    #[test]
    fn memory_type_unknown_returns_error() {
        let err = MemoryType::from_str("nonsense").unwrap_err();
        assert!(err.contains("nonsense"), "error should echo the bad input");
    }

    /// Objective: Verify that confidence/importance scores are clamped to [0, 1].
    /// Invariants: Values >1 become 1.0; values <0 become 0.0.
    #[test]
    fn score_is_clamped_to_unit_interval() {
        let exp = Experience::new("t1", MemoryType::Knowledge, "x", 1.7);
        assert!(
            (exp.confidence - 1.0).abs() < f64::EPSILON,
            "confidence above 1 should clamp to 1.0"
        );
        let mem = Memory::new("t1", MemoryType::Knowledge, "x", -0.5);
        assert!(
            mem.importance.abs() < f64::EPSILON,
            "importance below 0 should clamp to 0.0"
        );
    }

    /// Objective: Verify Memory default TTL is applied and expiry is correct.
    /// Invariants: expires_at == created_at + ttl, ttl > 0.
    #[test]
    fn memory_default_ttl_applied() {
        let mem = Memory::new("t1", MemoryType::Knowledge, "x", 0.5);
        assert!(mem.ttl.num_seconds() > 0, "TTL should be positive");
        let expected_expiry = mem.created_at + mem.ttl;
        assert_eq!(
            mem.expires_at, expected_expiry,
            "expiry must equal created + ttl"
        );
    }

    /// Objective: Verify is_expired uses expires_at correctly.
    /// Invariants: Memory not expired before expires_at; expired after.
    #[test]
    fn memory_is_expired_boundary() {
        let mem = Memory::new("t1", MemoryType::Interaction, "x", 0.5);
        let before_expiry = mem.expires_at - Duration::seconds(1);
        assert!(
            !mem.is_expired(before_expiry),
            "should not be expired before expiry"
        );
        assert!(
            mem.is_expired(mem.expires_at),
            "should be expired at expiry instant"
        );
    }

    /// Objective: Verify Metadata operations preserve insertion semantics.
    /// Invariants: insert returns previous; contains_key reflects state.
    #[test]
    fn metadata_insert_and_lookup() {
        let mut m = Metadata::default();
        assert!(
            !m.contains_key("k"),
            "empty metadata should not contain key"
        );
        assert!(m.is_empty(), "empty metadata should report is_empty");
        let prev = m.insert("k", serde_json::Value::Bool(true));
        assert!(prev.is_none(), "first insert should return None");
        assert_eq!(
            m.get("k"),
            Some(&serde_json::Value::Bool(true)),
            "get should return inserted value"
        );
    }

    /// Objective: Verify Message classification helpers distinguish roles.
    /// Invariants: is_user/is_assistant match the role string exactly.
    #[test]
    fn message_role_helpers() {
        let user = Message::new("user", "hi");
        let asst = Message::new("assistant", "hello");
        let sys = Message::new("system", "ctx");
        assert!(user.is_user() && !user.is_assistant(), "user role");
        assert!(!asst.is_user() && asst.is_assistant(), "assistant role");
        assert!(!sys.is_user() && !sys.is_assistant(), "system role");
    }

    /// Objective: Verify MemorySource round-trips through string form.
    /// Invariants: as_str -> FromStr is identity for all variants.
    #[test]
    fn memory_source_round_trip() {
        for variant in [
            MemorySource::Distillation,
            MemorySource::Manual,
            MemorySource::Feedback,
        ] {
            let s = variant.as_str();
            let back = MemorySource::from_str(s).expect("round trip");
            assert_eq!(variant, back, "variant {variant:?} should round-trip");
        }
    }

    /// Objective: Verify display_text prefers summary when present.
    /// Invariants: With empty summary, returns content; with summary, returns summary.
    #[test]
    fn display_text_prefers_summary() {
        let mut mem = Memory::new("t1", MemoryType::Knowledge, "full content", 0.5);
        assert_eq!(
            mem.display_text(),
            "full content",
            "empty summary should fall back to content"
        );
        mem.summary = "compressed".to_string();
        assert_eq!(
            mem.display_text(),
            "compressed",
            "non-empty summary should take precedence"
        );
    }
}
