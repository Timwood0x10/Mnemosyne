//! Relationship state store — the agent↔user bonding layer (阶段C-2, C3/C4).
//!
//! A **companion AI** needs to remember how close it has grown to a user over
//! time, not just *what* the user said. This module keeps a per
//! `(tenant, agent_entity, user_entity)` snapshot of the relationship:
//!
//! - `intimacy` — a scalar in `[0.0, 1.0]` that rises with positive user
//!   emotion and falls with negative user emotion.
//! - `stage` — a coarse relationship phase derived deterministically from
//!   `intimacy` thresholds (`stranger` → `acquaintance` → `companion` →
//!   `partner`).
//! - `emotion_trend` — whether the latest intimacy change is `rising`,
//!   `stable`, or `declining`.
//! - `recent_topics` — recurring keywords the user keeps coming back to.
//!
//! **No LLM**: every update is a pure keyword/rule computation. The state is
//! a *derived snapshot* that is upserted on every [`RelationshipStore::apply_messages`]
//! call; the underlying immutable facts remain in the fact store, so the
//! full conversation history is never lost (mem0 v3 ADD-only).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::fact_store::SqliteFactStore;
use crate::knowledge::companion_extract::extract_repeated_themes;
use crate::types::Message;

/// Increment applied to `intimacy` per user positive-emotion message.
const POSITIVE_DELTA: f64 = 0.02;
/// Increment applied to `intimacy` per user negative-emotion message.
const NEGATIVE_DELTA: f64 = 0.02;
/// Increment applied per assistant positive-emotion message. The agent's own
/// warm statements ("我很开心能认识你") also move the relationship — a
/// companion bond is mutual — but with a lighter weight than the user's
/// emotions, which remain the primary driver.
const AGENT_POSITIVE_DELTA: f64 = 0.01;
/// Decrement applied per assistant negative-emotion message (lighter than the
/// user's, mirroring [`AGENT_POSITIVE_DELTA`]).
const AGENT_NEGATIVE_DELTA: f64 = 0.01;
/// Maximum number of recurring topics kept in the snapshot.
const MAX_RECENT_TOPICS: usize = 10;

/// Keywords that signal a user positive-emotion message.
const POSITIVE_EMOTION_KEYWORDS: &[&str] = &[
    "开心",
    "高兴",
    "好开心",
    "快乐",
    "温暖",
    "信任",
    "喜欢",
    "爱",
    "感谢",
    "谢谢",
    "幸福",
    "安心",
    "欣慰",
    "期待",
    "依赖",
];

/// Keywords that signal a user negative-emotion message.
const NEGATIVE_EMOTION_KEYWORDS: &[&str] = &[
    "难过", "伤心", "生气", "讨厌", "害怕", "烦", "压力", "委屈", "失望", "痛苦", "焦虑", "愤怒",
    "孤独", "疲惫", "心累", "崩溃",
];

/// The coarse relationship phase, derived from `intimacy` thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipStage {
    /// A stranger with no established bond (`intimacy < 0.2`).
    Stranger,
    /// A passing acquaintance (`0.2 <= intimacy < 0.5`).
    Acquaintance,
    /// A trusted companion (`0.5 <= intimacy < 0.8`).
    Companion,
    /// A close partner (`intimacy >= 0.8`).
    Partner,
}

impl RelationshipStage {
    /// The stable string form stored in the database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            RelationshipStage::Stranger => "stranger",
            RelationshipStage::Acquaintance => "acquaintance",
            RelationshipStage::Companion => "companion",
            RelationshipStage::Partner => "partner",
        }
    }

    /// Map an `intimacy` value in `[0.0, 1.0]` to a stage via thresholds.
    #[must_use]
    pub const fn from_intimacy(intimacy: f64) -> Self {
        if intimacy < 0.2 {
            RelationshipStage::Stranger
        } else if intimacy < 0.5 {
            RelationshipStage::Acquaintance
        } else if intimacy < 0.8 {
            RelationshipStage::Companion
        } else {
            RelationshipStage::Partner
        }
    }
}

impl std::str::FromStr for RelationshipStage {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "stranger" => Ok(RelationshipStage::Stranger),
            "acquaintance" => Ok(RelationshipStage::Acquaintance),
            "companion" => Ok(RelationshipStage::Companion),
            "partner" => Ok(RelationshipStage::Partner),
            other => Err(format!("unknown relationship stage `{other}`")),
        }
    }
}

/// The direction of the latest intimacy change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmotionTrend {
    /// Intimacy increased on the last update.
    Rising,
    /// Intimacy did not move on the last update.
    Stable,
    /// Intimacy decreased on the last update.
    Declining,
}

impl EmotionTrend {
    /// The stable string form stored in the database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            EmotionTrend::Rising => "rising",
            EmotionTrend::Stable => "stable",
            EmotionTrend::Declining => "declining",
        }
    }

    /// Derive a trend from a signed intimacy delta.
    #[must_use]
    pub const fn from_delta(delta: f64) -> Self {
        if delta > 0.0 {
            EmotionTrend::Rising
        } else if delta < 0.0 {
            EmotionTrend::Declining
        } else {
            EmotionTrend::Stable
        }
    }
}

impl std::str::FromStr for EmotionTrend {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "rising" => Ok(EmotionTrend::Rising),
            "stable" => Ok(EmotionTrend::Stable),
            "declining" => Ok(EmotionTrend::Declining),
            other => Err(format!("unknown emotion trend `{other}`")),
        }
    }
}

/// A persisted relationship snapshot for one agent↔user pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationshipState {
    /// Tenant namespace for the relationship.
    pub tenant_id: String,
    /// The resolved Agent entity id.
    pub agent_entity_id: i64,
    /// The resolved User entity id.
    pub user_entity_id: i64,
    /// Intimacy in `[0.0, 1.0]`.
    pub intimacy: f64,
    /// Coarse relationship phase.
    pub stage: RelationshipStage,
    /// Direction of the latest intimacy change.
    pub emotion_trend: EmotionTrend,
    /// Recurring recent topics (repeated keywords).
    pub recent_topics: Vec<String>,
    /// Unix seconds of the last update.
    pub updated_at: i64,
}

impl RelationshipState {
    /// Build a fresh default snapshot for a previously-unseen pair.
    #[must_use]
    pub fn new(tenant_id: &str, agent_entity_id: i64, user_entity_id: i64) -> Self {
        Self {
            tenant_id: tenant_id.to_string(),
            agent_entity_id,
            user_entity_id,
            intimacy: 0.0,
            stage: RelationshipStage::Stranger,
            emotion_trend: EmotionTrend::Stable,
            recent_topics: Vec::new(),
            updated_at: unix_now(),
        }
    }
}

/// High-level relationship store built on the shared [`SqliteFactStore`].
///
/// It reuses the fact store's connection (and therefore its `relationship_state`
/// table) so that entity resolution and relationship persistence stay in one
/// consistent database.
pub struct RelationshipStore {
    store: Arc<SqliteFactStore>,
}

impl RelationshipStore {
    /// Construct a store over the shared fact store.
    #[must_use]
    pub fn new(store: Arc<SqliteFactStore>) -> Self {
        Self { store }
    }

    /// Read the current relationship snapshot for a pair.
    ///
    /// Returns `Ok(None)` when no relationship has been recorded yet.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the row cannot be read.
    pub fn get_relationship(
        &self,
        tenant_id: &str,
        agent_entity_id: i64,
        user_entity_id: i64,
    ) -> Result<Option<RelationshipState>> {
        self.store
            .load_relationship(tenant_id, agent_entity_id, user_entity_id)
    }

    /// Apply a batch of messages to the relationship using deterministic rules
    /// (C4), persisting the updated snapshot.
    ///
    /// Rules:
    /// - Every user positive-emotion message raises `intimacy` by `+0.02`;
    ///   every user negative-emotion message lowers it by `-0.02`, clamped to
    ///   `[0.0, 1.0]`.
    /// - `stage` is re-derived from `intimacy` via the stage thresholds.
    /// - `emotion_trend` is the direction of the latest intimacy change.
    /// - `recent_topics` are the recurring keywords across the messages.
    ///
    /// # Side effects
    ///
    /// Calling this resolves both entities via [`SqliteFactStore::resolve_agent`]
    /// and [`SqliteFactStore::resolve_user`]. If either does not exist yet, it
    /// is **implicitly created** so a relationship can be initialized on a
    /// brand-new pair; callers should therefore not be surprised that a
    /// `relationship_update` can materialize agent/user entity rows.
    ///
    /// # Errors
    ///
    /// Returns an error when entity resolution or persistence fails.
    pub fn apply_messages(
        &self,
        tenant_id: &str,
        agent_id: &str,
        user_id: &str,
        messages: &[Message],
    ) -> Result<RelationshipState> {
        let agent_entity_id = self.store.resolve_agent(tenant_id, agent_id)?;
        let user_entity_id = self.store.resolve_user(tenant_id, user_id)?;
        // Read-modify-write happens inside ONE lock critical section (see
        // `SqliteFactStore::update_relationship_atomic`): the previous
        // get-then-upsert pair ran under two separate locks, so concurrent
        // calls for the same (agent, user) pair both read the old intimacy,
        // each applied its own delta, and one increment was lost.
        self.store.update_relationship_atomic(
            tenant_id,
            agent_entity_id,
            user_entity_id,
            |current| {
                let current = current.cloned().unwrap_or_else(|| {
                    RelationshipState::new(tenant_id, agent_entity_id, user_entity_id)
                });
                let mut intimacy = current.intimacy;
                // Both sides of the conversation shape the bond: the user's
                // emotions are the primary driver (full delta); the agent's
                // own emotional statements count with a lighter weight.
                // Previously only user messages were considered, so an
                // assistant-heavy dialogue ("我很开心能认识你" / "我也很高兴")
                // left intimacy pinned at 0.0 forever.
                for msg in messages {
                    if msg.is_user() {
                        if has_positive_emotion(&msg.content) {
                            intimacy += POSITIVE_DELTA;
                        }
                        if has_negative_emotion(&msg.content) {
                            intimacy -= NEGATIVE_DELTA;
                        }
                    } else {
                        if has_positive_emotion(&msg.content) {
                            intimacy += AGENT_POSITIVE_DELTA;
                        }
                        if has_negative_emotion(&msg.content) {
                            intimacy -= AGENT_NEGATIVE_DELTA;
                        }
                    }
                }
                intimacy = intimacy.clamp(0.0, 1.0);

                RelationshipState {
                    tenant_id: tenant_id.to_string(),
                    agent_entity_id,
                    user_entity_id,
                    intimacy,
                    stage: RelationshipStage::from_intimacy(intimacy),
                    emotion_trend: EmotionTrend::from_delta(intimacy - current.intimacy),
                    recent_topics: extract_recent_topics(messages),
                    updated_at: unix_now(),
                }
            },
        )
    }
}

/// Whether a message carries a positive-emotion keyword.
fn has_positive_emotion(content: &str) -> bool {
    POSITIVE_EMOTION_KEYWORDS
        .iter()
        .any(|kw| content.contains(kw))
}

/// Whether a message carries a negative-emotion keyword.
fn has_negative_emotion(content: &str) -> bool {
    NEGATIVE_EMOTION_KEYWORDS
        .iter()
        .any(|kw| content.contains(kw))
}

/// Extract recurring topics (keywords appearing in ≥2 distinct turns), capped.
fn extract_recent_topics(messages: &[Message]) -> Vec<String> {
    extract_repeated_themes(messages)
        .into_iter()
        .map(|theme| theme.keyword)
        .take(MAX_RECENT_TOPICS)
        .collect()
}

/// Current unix time in seconds.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> RelationshipStore {
        RelationshipStore::new(Arc::new(
            SqliteFactStore::open_in_memory().expect("fact store"),
        ))
    }

    fn msgs(roles_and_contents: &[(&str, &str)]) -> Vec<Message> {
        roles_and_contents
            .iter()
            .map(|(r, c)| Message::new(*r, *c))
            .collect()
    }

    /// Objective: Verify positive user emotion raises intimacy and rises the
    /// trend; the state persists and is readable.
    /// Invariants: intimacy 0.0 → 0.04 after two "谢谢" messages; stage
    /// Stranger; trend Rising.
    #[test]
    fn positive_emotion_raises_intimacy() {
        let store = store();
        let messages = msgs(&[
            ("user", "谢谢你，今天真的很开心！"),
            ("user", "谢谢你的陪伴，让我感到温暖。"),
        ]);
        let state = store
            .apply_messages("tenant-a", "agent-bailiusu", "alice", &messages)
            .expect("apply messages");
        assert!(
            (state.intimacy - 0.04).abs() < 1e-9,
            "two positive messages should raise intimacy to 0.04, got {}",
            state.intimacy
        );
        assert_eq!(state.stage, RelationshipStage::Stranger);
        assert_eq!(state.emotion_trend, EmotionTrend::Rising);

        let loaded = store
            .get_relationship("tenant-a", state.agent_entity_id, state.user_entity_id)
            .expect("read persisted state")
            .expect("relationship exists after apply");
        assert!(
            (loaded.intimacy - 0.04).abs() < 1e-9,
            "persisted intimacy must match the applied value"
        );
    }

    /// Objective: Verify negative user emotion lowers intimacy and declines
    /// the trend.
    /// Invariants: intimacy 0.0 → 0.0 (clamped) after one negative message;
    /// trend Declining.
    #[test]
    fn negative_emotion_lowers_intimacy_and_clamps() {
        let store = store();
        let messages = msgs(&[("user", "我很伤心，觉得压力很大。")]);
        let state = store
            .apply_messages("tenant-a", "agent-bailiusu", "alice", &messages)
            .expect("apply messages");
        assert!(
            state.intimacy >= 0.0,
            "intimacy must never go below zero, got {}",
            state.intimacy
        );
        // Starting from 0.0, after clamping we still have 0.0 → delta is zero → Stable
        assert_eq!(state.emotion_trend, EmotionTrend::Stable);
    }

    /// Objective: Verify the AGENT's own emotional statements also move the
    /// relationship — a companion bond is mutual. Previously only user
    /// messages were counted, so an assistant-heavy warm dialogue left
    /// intimacy pinned at 0.0 forever (the defect this fixes).
    /// Invariants: three assistant positive-emotion messages raise intimacy
    /// by 3 × AGENT_POSITIVE_DELTA; user messages still carry full weight.
    #[test]
    fn assistant_emotion_drives_intimacy() {
        let store = store();
        let messages = msgs(&[
            ("assistant", "我很开心能认识你，谢谢你信任我。"),
            ("assistant", "我也很高兴能陪着你。"),
            ("assistant", "我们的相处让我感到温暖，真的很珍惜。"),
        ]);
        let state = store
            .apply_messages("tenant-a", "agent-bailiusu", "bob", &messages)
            .expect("apply messages");
        let expected = 3.0 * AGENT_POSITIVE_DELTA;
        assert!(
            (state.intimacy - expected).abs() < 1e-9,
            "three assistant positive messages should raise intimacy to {expected}, got {}",
            state.intimacy
        );
        assert_eq!(
            state.emotion_trend,
            EmotionTrend::Rising,
            "agent warmth must move the trend, not stay stable"
        );
    }

    /// Objective: Verify mixed user + assistant emotions combine: user
    /// messages count with full delta, assistant messages with the lighter
    /// agent delta.
    /// Invariants: one user positive (+0.02) + one assistant positive (+0.01)
    /// → intimacy 0.03.
    #[test]
    fn mixed_user_and_agent_emotion_combine() {
        let store = store();
        let messages = msgs(&[
            ("user", "谢谢你，今天很开心！"),
            ("assistant", "我也很高兴能帮你。"),
        ]);
        let state = store
            .apply_messages("tenant-a", "agent-bailiusu", "carol", &messages)
            .expect("apply messages");
        let expected = POSITIVE_DELTA + AGENT_POSITIVE_DELTA;
        assert!(
            (state.intimacy - expected).abs() < 1e-9,
            "user + agent positive should sum to {expected}, got {}",
            state.intimacy
        );
    }

    /// Objective: Verify intimacy is clamped to `[0, 1]` and the stage mapping
    /// follows the thresholds.
    /// Invariants: intimacy 0.9 → Partner; 0.3 → Acquaintance; 0.6 → Companion.
    #[test]
    fn stage_mapping_follows_thresholds() {
        assert_eq!(
            RelationshipStage::from_intimacy(0.1),
            RelationshipStage::Stranger
        );
        assert_eq!(
            RelationshipStage::from_intimacy(0.3),
            RelationshipStage::Acquaintance
        );
        assert_eq!(
            RelationshipStage::from_intimacy(0.6),
            RelationshipStage::Companion
        );
        assert_eq!(
            RelationshipStage::from_intimacy(0.9),
            RelationshipStage::Partner
        );
        assert_eq!(
            RelationshipStage::from_intimacy(1.5),
            RelationshipStage::Partner
        );
        assert_eq!(
            RelationshipStage::from_intimacy(-0.5),
            RelationshipStage::Stranger
        );
    }

    /// Objective: Verify recurring topics are captured from the messages.
    /// Invariants: "工作" mentioned across two turns becomes a recent topic.
    #[test]
    fn recent_topics_are_recurring_keywords() {
        let store = store();
        let messages = msgs(&[
            ("user", "工作的事烦死了"),
            ("user", "工作又压了一堆"),
            ("assistant", "你工作别太拼了"),
        ]);
        let state = store
            .apply_messages("tenant-a", "agent-bailiusu", "alice", &messages)
            .expect("apply messages");
        assert!(
            state.recent_topics.iter().any(|t| t == "工作"),
            "recurring topic 工作 must be captured, got {:?}",
            state.recent_topics
        );
    }

    /// Objective: Verify the trend is computed from the intimacy delta between
    /// two consecutive updates.
    /// Invariants: a positive update after a prior positive update stays
    /// Rising; a negative follow-up flips it to Declining.
    #[test]
    fn trend_tracks_delta_between_updates() {
        let store = store();
        let positive = msgs(&[("user", "谢谢你，我很开心")]);
        let negative = msgs(&[("user", "我很伤心")]);

        let first = store
            .apply_messages("tenant-a", "agent-bailiusu", "alice", &positive)
            .expect("first apply");
        assert_eq!(first.emotion_trend, EmotionTrend::Rising);

        let second = store
            .apply_messages("tenant-a", "agent-bailiusu", "alice", &negative)
            .expect("second apply");
        assert_eq!(
            second.emotion_trend,
            EmotionTrend::Declining,
            "a negative update after a positive one must be declining"
        );
    }
}
