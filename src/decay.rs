//! Deterministic memory decay / forgetting management.
//!
//! Decay reduces a stored fact's retrieval influence over time, by importance,
//! or by access frequency. It is a pure, deterministic policy — no LLM is
//! consulted. The critical invariant is that decay **never deletes** a fact:
//! it only writes back a `weight` and an `archived` flag, so the persona
//! evolution timeline remains fully reconstructable (mem0 v3 ADD-only).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::agent_personality::AGENT_PERSONALITY_ATTRIBUTION;
use crate::cognition::{Fact, FactStore, FactType};
use crate::error::{Error, Result};
use crate::fact_store::SqliteFactStore;

/// Default config path used when the `DECAY_CONFIG_PATH` env var is unset.
pub const DECAY_CONFIG_PATH: &str = "config/decay_config.json";

/// Seconds per day, used to convert a fact's age into days.
const SECONDS_PER_DAY: f64 = 86_400.0;

/// Selection of the decay policy applied to facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DecayStrategy {
    /// Decay by age of the fact (created_at).
    #[default]
    TimeBased,
    /// Decay by lower importance.
    ImportanceBased,
    /// Decay by low access frequency.
    AccessFrequencyBased,
    /// Combine all three signals.
    Hybrid,
}

impl std::str::FromStr for DecayStrategy {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "time_based" | "timebased" | "time" => Ok(DecayStrategy::TimeBased),
            "importance_based" | "importancebased" | "importance" => {
                Ok(DecayStrategy::ImportanceBased)
            }
            "access_frequency_based" | "accessfrequencybased" | "access" => {
                Ok(DecayStrategy::AccessFrequencyBased)
            }
            "hybrid" => Ok(DecayStrategy::Hybrid),
            other => Err(format!("unknown decay strategy: {other}")),
        }
    }
}

/// Tunable knobs for the decay policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DecayConfig {
    /// Which policy drives the decay score.
    pub strategy: DecayStrategy,
    /// Facts older than this many days are down-weighted (time decay).
    pub time_to_live_days: u64,
    /// Facts with importance below this are down-weighted (default 0.5 when absent).
    pub importance_threshold: f64,
    /// Facts accessed fewer than this many times are down-weighted.
    pub access_threshold: u64,
    /// The down-weight multiplier applied to a decayed fact (e.g. 0.5 = half weight).
    pub decay_factor: f64,
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self {
            strategy: DecayStrategy::Hybrid,
            time_to_live_days: 90,
            importance_threshold: 0.3,
            access_threshold: 5,
            decay_factor: 0.5,
        }
    }
}

impl DecayConfig {
    /// Load the config from `DECAY_CONFIG_PATH` (default `config/decay_config.json`).
    /// A missing or unreadable file silently falls back to defaults so the
    /// server always runs.
    #[must_use]
    pub fn load() -> Self {
        let path =
            std::env::var("DECAY_CONFIG_PATH").unwrap_or_else(|_| DECAY_CONFIG_PATH.to_string());
        match Self::load_from_path(&path) {
            Ok(cfg) => cfg,
            Err(err) => {
                tracing::warn!("decay config unavailable at {path}: {err}; using defaults");
                Self::default()
            }
        }
    }

    /// Load from a JSON file. A missing file yields defaults (not an error).
    ///
    /// # Errors
    ///
    /// Returns a config error when the file exists but is invalid JSON or
    /// fails validation.
    pub fn load_from_path(path: &str) -> Result<Self> {
        if !Path::new(path).exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)?;
        let cfg: DecayConfig = serde_json::from_str(&raw)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Validate numeric bounds.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when a fraction is outside `[0, 1]`.
    pub fn validate(&self) -> Result<()> {
        if !(0.0..=1.0).contains(&self.importance_threshold) {
            return Err(Error::Config(format!(
                "importance_threshold must be in [0,1], got {}",
                self.importance_threshold
            )));
        }
        if !(0.0..=1.0).contains(&self.decay_factor) {
            return Err(Error::Config(format!(
                "decay_factor must be in [0,1], got {}",
                self.decay_factor
            )));
        }
        Ok(())
    }
}

/// Outcome of evaluating decay for a single fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecayAssessment {
    /// The fact this assessment applies to.
    pub fact_id: i64,
    /// Retrieval influence in `[0, 1]`; 1.0 is fully fresh, closer to 0 is decayed.
    pub decay_score: f64,
    /// True when the fact should be down-weighted / archived (never deleted).
    pub should_archive: bool,
    /// Human-readable reason for the decision.
    pub reason: String,
}

/// Returns `true` for high-value persona facts that must never decay:
/// `agent_personality` attribution or `Relationship` facts.
#[must_use]
pub fn is_high_value(fact: &Fact) -> bool {
    fact.fact_type == FactType::Relationship
        || fact.payload.get("attribution").and_then(|v| v.as_str())
            == Some(AGENT_PERSONALITY_ATTRIBUTION)
}

/// Compute the decay assessment for a fact under the given policy.
///
/// High-value persona facts are protected: they always score 1.0 and are never
/// archived, so the persona never breaks.
#[must_use]
pub fn compute_decay(fact: &Fact, config: &DecayConfig, now: i64) -> DecayAssessment {
    compute_decay_inner(fact, config, now, false)
}

/// Compute decay while ignoring the high-value protection (used when `force`
/// is set on the tool). Protected facts are still returned candidly.
#[must_use]
pub fn compute_decay_with_force(
    fact: &Fact,
    config: &DecayConfig,
    now: i64,
    force: bool,
) -> DecayAssessment {
    compute_decay_inner(fact, config, now, force)
}

fn compute_decay_inner(
    fact: &Fact,
    config: &DecayConfig,
    now: i64,
    force: bool,
) -> DecayAssessment {
    let fact_id = fact.id.unwrap_or(0);
    if !force && is_high_value(fact) {
        return DecayAssessment {
            fact_id,
            decay_score: 1.0,
            should_archive: false,
            reason: "high_value_persona".into(),
        };
    }

    let age_days = (now.saturating_sub(fact.created_at)) as f64 / SECONDS_PER_DAY;
    let importance = extract_importance(fact);
    let access_count = extract_access_count(fact);

    let decay_score = match config.strategy {
        DecayStrategy::TimeBased => time_score(age_days, config),
        DecayStrategy::ImportanceBased => importance_score(importance, config),
        DecayStrategy::AccessFrequencyBased => access_score(access_count, config),
        DecayStrategy::Hybrid => {
            time_score(age_days, config)
                * importance_score(importance, config)
                * access_score(access_count, config)
        }
    }
    .clamp(0.0, 1.0);

    let should_archive = decay_score < 1.0;
    let reason = if should_archive {
        format!("decay_score={decay_score:.3}")
    } else {
        "fresh".to_string()
    };
    DecayAssessment {
        fact_id,
        decay_score,
        should_archive,
        reason,
    }
}

/// Time-based score: fresh within TTL, then exponential falloff toward
/// `decay_factor` past the window.
fn time_score(age_days: f64, config: &DecayConfig) -> f64 {
    let ttl = config.time_to_live_days as f64;
    if age_days <= ttl {
        return 1.0;
    }
    let excess = age_days - ttl;
    let half_life = (ttl / 2.0).max(1.0);
    let decay = (0.5_f64).powf(excess / half_life);
    1.0 - (1.0 - decay) * (1.0 - config.decay_factor)
}

/// Importance-based score: above threshold stays 1.0, below scales down to
/// `decay_factor`.
fn importance_score(importance: f64, config: &DecayConfig) -> f64 {
    if importance >= config.importance_threshold {
        return 1.0;
    }
    let base = (importance / config.importance_threshold.max(1e-9)).max(0.0);
    1.0 - (1.0 - base) * (1.0 - config.decay_factor)
}

/// Access-frequency score: at/above threshold stays 1.0, below scales down to
/// `decay_factor`.
fn access_score(access_count: u64, config: &DecayConfig) -> f64 {
    if access_count >= config.access_threshold {
        return 1.0;
    }
    let base = (access_count as f64) / (config.access_threshold.max(1) as f64);
    1.0 - (1.0 - base) * (1.0 - config.decay_factor)
}

/// Read the optional `importance` field from the fact payload (default 0.5).
fn extract_importance(fact: &Fact) -> f64 {
    fact.payload
        .get("importance")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.5)
}

/// Read the optional `access_count` field from the fact payload (default 0).
fn extract_access_count(fact: &Fact) -> u64 {
    fact.payload
        .get("access_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

/// Aggregate statistics produced by a decay pass.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct DecayStats {
    /// Total facts examined.
    pub scanned: u64,
    /// Facts that were down-weighted / archived.
    pub archived: u64,
    /// Facts that stayed fresh (not archived).
    pub kept: u64,
    /// Facts protected (not archived) because they are high-value persona.
    pub high_value_protected: u64,
}

/// Run one decay pass over the chosen entity (or every entity when `entity_id`
/// is `None`), writing back decay scores / archive flags. Never deletes rows.
///
/// # Errors
///
/// Returns a storage error when any read or write in the pass fails.
pub fn run_decay_pass(
    store: &SqliteFactStore,
    config: &DecayConfig,
    entity_id: Option<i64>,
    force: bool,
    now: i64,
) -> Result<DecayStats> {
    let entity_ids = match entity_id {
        Some(id) => vec![id],
        None => store.all_entity_ids()?,
    };
    let mut stats = DecayStats::default();
    for entity_id in entity_ids {
        let facts = store.get_facts(entity_id)?;
        for fact in facts {
            let Some(fact_id) = fact.id else { continue };
            stats.scanned += 1;
            if !force && is_high_value(&fact) {
                stats.high_value_protected += 1;
                stats.kept += 1;
                continue;
            }
            let assessment = compute_decay_with_force(&fact, config, now, force);
            if assessment.should_archive {
                store.set_decay(fact_id, assessment.decay_score, true)?;
                stats.archived += 1;
            } else {
                stats.kept += 1;
            }
        }
    }
    Ok(stats)
}

/// Mark a fact as archived and write back its decay score. Never deletes the row.
///
/// # Errors
///
/// Returns a storage error when the update fails.
pub fn archive_fact(store: &SqliteFactStore, fact_id: i64, score: f64) -> Result<()> {
    store.set_decay(fact_id, score, true)
}

/// List the archived (down-weighted, still present) facts for an entity.
///
/// This proves decay never deletes: archived facts remain fully readable.
///
/// # Errors
///
/// Returns a storage error when the read fails.
pub fn list_archived(store: &SqliteFactStore, entity_id: i64) -> Result<Vec<Fact>> {
    store.list_archived(entity_id)
}

/// Run the background decay task (plan D2): every `interval` run one decay
/// pass over all entities, until the caller sets `stop` to `true`. The decay
/// policy itself is deterministic and stateless, so each tick is independent.
///
/// A shared `Arc<AtomicBool>` stop flag lets a host shutdown the loop cleanly
/// between ticks (never mid-write). Spawn it with
/// `tokio::spawn(run_decay_loop(store, config, interval, stop.clone()))`.
///
/// # Errors
///
/// Returns the first storage error raised by a decay pass; the loop otherwise
/// runs forever (until `stop`).
pub async fn run_decay_loop(
    store: Arc<SqliteFactStore>,
    config: Arc<DecayConfig>,
    interval: std::time::Duration,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let mut ticker = tokio::time::interval(interval);
    // The first tick completes immediately, so one pass runs right away, then
    // every `interval` thereafter.
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        ticker.tick().await;
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Error::Internal(e.to_string()))?
            .as_secs() as i64;
        run_decay_pass(&store, &config, None, false, now)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_personality::AGENT_PERSONALITY_ATTRIBUTION;
    use crate::cognition::FactType;
    use crate::fact_store::SqliteFactStore;
    use serde_json::{Value, json};

    fn fact(id: Option<i64>, fact_type: FactType, created_at: i64, payload: Value) -> Fact {
        Fact {
            id,
            entity_id: 7,
            fact_type,
            time: 1,
            payload,
            evidence_id: None,
            created_at,
        }
    }

    /// Objective: Verify time decay keeps a fresh fact at full weight and
    /// archives a very old fact (score approaches `decay_factor`).
    /// Invariants: age <= TTL → score 1.0, not archived; huge age → score ≈
    /// decay_factor, archived, reason mentions the score.
    #[test]
    fn time_decay_keeps_fresh_and_archives_old() {
        let config = DecayConfig {
            strategy: DecayStrategy::TimeBased,
            ..DecayConfig::default()
        };
        let now = 1_000_000;

        let fresh = compute_decay(
            &fact(Some(17), FactType::Event, now, json!({"importance": 0.9})),
            &config,
            now,
        );
        assert_eq!(fresh.decay_score, 1.0, "fresh fact keeps full weight");
        assert!(!fresh.should_archive, "fresh fact is not archived");

        let old = compute_decay(
            &fact(Some(17), FactType::Event, now, json!({"importance": 0.9})),
            &config,
            now + 2000 * 86_400,
        );
        assert!(old.should_archive, "very old fact is archived");
        assert!(old.decay_score < 1.0, "buried fact loses weight");
        assert!(
            (old.decay_score - config.decay_factor).abs() < 0.05,
            "very old facts approach decay_factor, got {}",
            old.decay_score
        );
        assert!(old.reason.contains("decay_score"), "reason names the score");
    }

    /// Objective: Verify importance decay archives low-importance facts and
    /// keeps high-importance facts.
    /// Invariants: importance below threshold → archived; above → full weight.
    #[test]
    fn importance_decay_archives_low_importance() {
        let config = DecayConfig {
            strategy: DecayStrategy::ImportanceBased,
            ..DecayConfig::default()
        };
        let now = 1_000_000;

        let low = compute_decay(
            &fact(Some(17), FactType::Event, now, json!({"importance": 0.1})),
            &config,
            now,
        );
        assert!(low.should_archive, "low importance is archived");
        assert!(low.decay_score < 1.0);

        let high = compute_decay(
            &fact(Some(17), FactType::Event, now, json!({"importance": 0.9})),
            &config,
            now,
        );
        assert!(!high.should_archive, "high importance stays fresh");
        assert_eq!(high.decay_score, 1.0);
    }

    /// Objective: Verify the hybrid strategy combines all three signals — only
    /// a fact that is fresh, important, and frequently accessed stays fresh.
    /// Invariants: all-fresh → 1.0; stale+low → archived.
    #[test]
    fn hybrid_decay_combines_signals() {
        let config = DecayConfig::default(); // Hybrid
        let now = 1_000_000;

        let news = compute_decay(
            &fact(
                Some(17),
                FactType::Event,
                now,
                json!({"importance": 0.9, "access_count": 10}),
            ),
            &config,
            now,
        );
        assert!(!news.should_archive, "fresh+important+accessed stays fresh");
        assert_eq!(news.decay_score, 1.0);

        let stale = compute_decay(
            &fact(
                Some(17),
                FactType::Event,
                now,
                json!({"importance": 0.1, "access_count": 0}),
            ),
            &config,
            now + 2000 * 86_400,
        );
        assert!(stale.should_archive, "stale+low+unaccessed is archived");
    }

    /// Objective: Verify high-value persona facts never decay even when very
    /// old — `agent_personality` attribution and `Relationship` facts.
    /// Invariants: both score 1.0, not archived, reason "high_value_persona".
    #[test]
    fn high_value_persona_facts_never_decay() {
        let config = DecayConfig::default();
        let now = 1_000_000 + 5000 * 86_400;

        let persona = compute_decay(
            &fact(
                Some(17),
                FactType::Preference,
                1_000,
                json!({"attribution": AGENT_PERSONALITY_ATTRIBUTION}),
            ),
            &config,
            now,
        );
        assert_eq!(persona.decay_score, 1.0, "persona fact keeps full weight");
        assert!(!persona.should_archive, "persona fact is never archived");
        assert_eq!(persona.reason, "high_value_persona");

        let relationship = compute_decay(
            &fact(
                Some(17),
                FactType::Relationship,
                1_000,
                json!({"target": "Bob"}),
            ),
            &config,
            now,
        );
        assert_eq!(
            relationship.decay_score, 1.0,
            "relationship fact never decays"
        );
        assert!(!relationship.should_archive);
    }

    /// Objective: Verify `force` ignores the high-value protection and decays
    /// a persona fact that is otherwise protected.
    /// Invariants: with force=true the old persona fact is archived.
    #[test]
    fn force_overrides_high_value_protection() {
        let config = DecayConfig::default();
        let now = 1_000_000 + 5000 * 86_400;
        let persona = fact(
            Some(17),
            FactType::Preference,
            1_000,
            json!({"attribution": AGENT_PERSONALITY_ATTRIBUTION}),
        );
        let forced = compute_decay_with_force(&persona, &config, now, true);
        assert!(forced.should_archive, "force decays the protected fact");
        assert!(forced.decay_score < 1.0);
    }

    /// Objective: Verify archiving only writes flags — the fact row is never
    /// deleted and remains readable both via `list_archived` and the full set.
    /// Invariants: archived fact still present, archived flag set, weight set.
    #[test]
    fn archive_preserves_the_fact_row() {
        let store = SqliteFactStore::open_in_memory().expect("fact store");
        let id = store
            .insert_fact(&fact(
                None,
                FactType::Event,
                1_000_000,
                json!({"importance": 0.9}),
            ))
            .expect("insert fact");
        assert!(id > 0, "insert returns a positive id");

        archive_fact(&store, id, 0.5).expect("archive fact");

        let archived = list_archived(&store, 7).expect("list archived");
        assert_eq!(archived.len(), 1, "archived fact remains readable");
        assert_eq!(
            archived[0].id,
            Some(id),
            "the archived fact is the same row"
        );

        let all = store.get_facts(7).expect("get all facts");
        assert_eq!(all.len(), 1, "nothing was deleted from the full set");

        let (weight, archived) = store.get_decay(id).expect("read decay flags");
        assert_eq!(weight, 0.5, "weight written back");
        assert!(archived, "archived flag set");
    }

    /// Objective: Verify a full decay pass aggregates statistics correctly,
    /// including high-value protection and scan-all-entities behavior.
    /// Invariants: scanned == total, archived counts decayed facts, protected
    /// persona facts are counted and kept.
    #[test]
    fn decay_pass_aggregates_statistics() {
        let store = SqliteFactStore::open_in_memory().expect("fact store");
        let eid = store
            .resolve_user("tenant-a", "alice")
            .expect("resolve user");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64;
        // Two old, decayable events and one old persona fact (protected).
        for _ in 0..2 {
            store
                .insert_fact(&Fact {
                    id: None,
                    entity_id: eid,
                    fact_type: FactType::Event,
                    time: 1,
                    payload: json!({"importance": 0.9}),
                    evidence_id: None,
                    created_at: 0,
                })
                .expect("insert old event");
        }
        store
            .insert_fact(&Fact {
                id: None,
                entity_id: eid,
                fact_type: FactType::Preference,
                time: 1,
                payload: json!({"attribution": AGENT_PERSONALITY_ATTRIBUTION}),
                evidence_id: None,
                created_at: 0,
            })
            .expect("insert persona fact");

        let stats = run_decay_pass(&store, &DecayConfig::default(), Some(eid), false, now)
            .expect("decay pass");
        assert_eq!(stats.scanned, 3, "all facts scanned");
        assert_eq!(stats.archived, 2, "both old events archived");
        assert_eq!(stats.kept, 1, "the protected persona fact is kept");
        assert_eq!(stats.high_value_protected, 1, "persona fact protected");
    }

    /// Objective: Verify the fallback config loads when the file is missing.
    /// Invariants: defaults are returned and no error is raised.
    #[test]
    fn default_config_loads_when_file_missing() {
        let cfg = DecayConfig::load_from_path("/nonexistent/decay.json").expect("defaults");
        assert_eq!(cfg.strategy, DecayStrategy::Hybrid);
        assert_eq!(cfg.time_to_live_days, 90);
        assert_eq!(cfg.importance_threshold, 0.3);
        assert_eq!(cfg.access_threshold, 5);
        assert_eq!(cfg.decay_factor, 0.5);
    }

    /// Objective: Verify the background decay task (D2) runs a pass and exits
    /// cleanly when the stop flag is set.
    /// Invariants: after `run_decay_loop` returns Ok, the loop stopped without
    /// error; a decayable fact was archived by the single tick.
    #[tokio::test]
    async fn background_decay_loop_runs_and_stops() {
        let store = SqliteFactStore::open_in_memory().expect("fact store");
        let eid = store.resolve_user("tenant-a", "bob").expect("resolve user");
        // One very old event → should be archived by the first tick.
        store
            .insert_fact(&Fact {
                id: None,
                entity_id: eid,
                fact_type: FactType::Event,
                time: 1,
                payload: json!({"importance": 0.9}),
                evidence_id: None,
                created_at: 0,
            })
            .expect("insert old event");

        let stop = Arc::new(AtomicBool::new(false));
        let stop_task = stop.clone();
        let cfg = Arc::new(DecayConfig {
            strategy: DecayStrategy::TimeBased,
            ..DecayConfig::default()
        });
        let store = Arc::new(store);
        let handle = tokio::spawn(run_decay_loop(
            store.clone(),
            cfg,
            std::time::Duration::from_millis(20),
            stop_task,
        ));

        // Let the first tick run, then ask the loop to stop.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        stop.store(true, Ordering::Relaxed);
        handle.await.expect("join").expect("loop must exit Ok");

        let (_, archived) = store
            .get_decay(store.get_facts(eid).expect("facts")[0].id.expect("id"))
            .expect("flags");
        assert!(archived, "the first tick archived the stale event");
    }

    /// Objective: Verify a JSON config file is parsed and validated.
    /// Invariants: strategy and numeric overrides are honored.
    #[test]
    fn config_loads_from_temp_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("decay.json");
        std::fs::write(
            &path,
            r#"{"strategy":"time_based","time_to_live_days":30,"importance_threshold":0.4,"access_threshold":3,"decay_factor":0.7}"#,
        )
        .expect("write config");
        let cfg = DecayConfig::load_from_path(path.to_str().expect("path")).expect("load");
        assert_eq!(cfg.strategy, DecayStrategy::TimeBased);
        assert_eq!(cfg.time_to_live_days, 30);
        assert_eq!(cfg.importance_threshold, 0.4);
        assert_eq!(cfg.access_threshold, 3);
        assert_eq!(cfg.decay_factor, 0.7);
    }
}
