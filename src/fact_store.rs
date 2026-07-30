//! FactStore — SQLite persistence layer for Facts.
//!
//! Implements the [`FactStore`] trait using SQLite. Facts are stored in a
//! single `facts` table with a JSON `payload` column for flexible schemas.
//!
//! ## Schema
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS facts (
//!     id           INTEGER PRIMARY KEY AUTOINCREMENT,
//!     entity_id    INTEGER NOT NULL,
//!     fact_type    TEXT NOT NULL,
//!     time         INTEGER NOT NULL,
//!     payload      TEXT NOT NULL,   -- JSON
//!     evidence_id  INTEGER,
//!     created_at   INTEGER NOT NULL DEFAULT (strftime('%s','now'))
//! );
//! CREATE INDEX idx_facts_entity ON facts(entity_id);
//! CREATE INDEX idx_facts_type ON facts(entity_id, fact_type);
//! CREATE INDEX idx_facts_time ON facts(entity_id, time);
//! ```

use std::sync::Mutex;

use rusqlite::{Connection, params};

use crate::cognition::{Fact, FactStore, FactType};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS facts (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id    INTEGER NOT NULL,
    fact_type    TEXT NOT NULL,
    time         INTEGER NOT NULL,
    payload      TEXT NOT NULL,
    evidence_id  INTEGER,
    created_at   INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE INDEX IF NOT EXISTS idx_facts_entity ON facts(entity_id);
CREATE INDEX IF NOT EXISTS idx_facts_type ON facts(entity_id, fact_type);
CREATE INDEX IF NOT EXISTS idx_facts_time ON facts(entity_id, time);
";

/// SQLite-backed fact store.
pub struct SqliteFactStore {
    conn: Mutex<Connection>,
}

impl SqliteFactStore {
    /// Open (or create) a fact store at the given path.
    pub fn open(path: &str) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(SqliteFactStore {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory fact store (for testing).
    pub fn open_in_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(SqliteFactStore {
            conn: Mutex::new(conn),
        })
    }

    /// Convert a database row to a Fact.
    fn row_to_fact(row: &rusqlite::Row) -> rusqlite::Result<Fact> {
        let type_str: String = row.get("fact_type")?;
        let fact_type = match type_str.as_str() {
            "identity" => FactType::Identity,
            "preference" => FactType::Preference,
            "goal" => FactType::Goal,
            "event" => FactType::Event,
            "relationship" => FactType::Relationship,
            "emotion" => FactType::Emotion,
            "location" => FactType::Location,
            "occupation" => FactType::Occupation,
            "interest" => FactType::Interest,
            "habit" => FactType::Habit,
            _ => FactType::Event,
        };
        let payload_str: String = row.get("payload")?;
        let payload: serde_json::Value =
            serde_json::from_str(&payload_str).unwrap_or(serde_json::Value::Null);

        Ok(Fact {
            id: Some(row.get("id")?),
            entity_id: row.get("entity_id")?,
            fact_type,
            time: row.get("time")?,
            payload,
            evidence_id: row.get("evidence_id")?,
            created_at: row.get("created_at")?,
        })
    }
}

impl FactStore for SqliteFactStore {
    fn insert_fact(&self, fact: &Fact) -> i64 {
        let type_str = format!("{:?}", fact.fact_type).to_lowercase();
        let payload_str = serde_json::to_string(&fact.payload).unwrap_or_default();
        self.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO facts (entity_id, fact_type, time, payload, evidence_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    fact.entity_id,
                    type_str,
                    fact.time,
                    payload_str,
                    fact.evidence_id,
                    fact.created_at,
                ],
            )
            .unwrap_or(0);
        self.conn.lock().unwrap().last_insert_rowid()
    }

    fn insert_batch(&self, facts: &[Fact]) -> usize {
        if facts.is_empty() {
            return 0;
        }
        // Use a transaction for batch inserts
        self.conn.lock().unwrap().execute_batch("BEGIN;").ok();
        let mut count = 0usize;
        for fact in facts {
            if self.insert_fact(fact) > 0 {
                count += 1;
            }
        }
        self.conn.lock().unwrap().execute_batch("COMMIT;").ok();
        count
    }

    fn get_facts(&self, entity_id: i64) -> Vec<Fact> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, entity_id, fact_type, time, payload, evidence_id, created_at FROM facts WHERE entity_id = ?1 ORDER BY time")
            .unwrap();
        let rows = stmt
            .query_map(params![entity_id], Self::row_to_fact)
            .unwrap();
        rows.filter_map(|r| r.ok()).collect()
    }

    fn get_facts_by_type(&self, entity_id: i64, fact_type: FactType) -> Vec<Fact> {
        let conn = self.conn.lock().unwrap();
        let type_str = format!("{:?}", fact_type).to_lowercase();
        let mut stmt = conn
            .prepare("SELECT id, entity_id, fact_type, time, payload, evidence_id, created_at FROM facts WHERE entity_id = ?1 AND fact_type = ?2 ORDER BY time")
            .unwrap();
        let rows = stmt
            .query_map(params![entity_id, type_str], Self::row_to_fact)
            .unwrap();
        rows.filter_map(|r| r.ok()).collect()
    }

    fn get_timeline(&self, entity_id: i64) -> Vec<Fact> {
        // Timeline is just facts sorted by time (newest first for display)
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, entity_id, fact_type, time, payload, evidence_id, created_at FROM facts WHERE entity_id = ?1 ORDER BY time DESC")
            .unwrap();
        let rows = stmt
            .query_map(params![entity_id], Self::row_to_fact)
            .unwrap();
        rows.filter_map(|r| r.ok()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_fact(entity_id: i64, ft: FactType) -> Fact {
        Fact {
            id: None,
            entity_id,
            fact_type: ft,
            time: 2026,
            payload: serde_json::json!({"test": true}),
            evidence_id: None,
            created_at: 0,
        }
    }

    /// Objective: Verify that a fact inserted via insert_fact can be retrieved.
    /// Invariants: After inserting and getting, the payload matches.
    #[test]
    fn insert_and_retrieve() {
        let store = SqliteFactStore::open_in_memory().unwrap();
        let fact = sample_fact(10001, FactType::Event);
        let id = store.insert_fact(&fact);
        assert!(id > 0, "insert should return a positive id");

        let facts = store.get_facts(10001);
        assert_eq!(facts.len(), 1, "should retrieve exactly one fact");
        assert_eq!(facts[0].entity_id, 10001);
        assert_eq!(facts[0].fact_type, FactType::Event);
    }

    /// Objective: Verify that batch insert inserts all facts in a single
    /// transaction.
    /// Invariants: All 5 facts are retrievable after batch insert.
    #[test]
    fn batch_insert_commits_all() {
        let store = SqliteFactStore::open_in_memory().unwrap();
        let facts: Vec<Fact> = (0..5)
            .map(|i| sample_fact(10001 + i, FactType::Preference))
            .collect();
        let n = store.insert_batch(&facts);
        assert_eq!(n, 5, "batch insert should return count of inserted rows");

        for i in 0..5 {
            let f = store.get_facts(10001 + i);
            assert_eq!(f.len(), 1, "entity {} should have one fact", 10001 + i);
        }
    }

    /// Objective: Verify get_facts_by_type returns only matching facts.
    /// Invariants: Entity with 1 Event + 1 Preference returns 1 each.
    #[test]
    fn filter_by_type() {
        let store = SqliteFactStore::open_in_memory().unwrap();
        store.insert_fact(&sample_fact(42, FactType::Event));
        store.insert_fact(&sample_fact(42, FactType::Preference));

        let events = store.get_facts_by_type(42, FactType::Event);
        assert_eq!(events.len(), 1, "should find exactly 1 event fact");
        assert_eq!(events[0].fact_type, FactType::Event);

        let prefs = store.get_facts_by_type(42, FactType::Preference);
        assert_eq!(prefs.len(), 1, "should find exactly 1 preference fact");
    }

    /// Objective: Verify timeline is sorted by time descending.
    /// Invariants: Facts from 2024, 2025, 2026 come back in reverse order.
    #[test]
    fn timeline_is_sorted_descending() {
        let store = SqliteFactStore::open_in_memory().unwrap();
        for year in &[2024i32, 2025, 2026] {
            let mut f = sample_fact(1, FactType::Event);
            f.time = *year;
            store.insert_fact(&f);
        }
        let tl = store.get_timeline(1);
        assert_eq!(tl.len(), 3, "timeline should have 3 entries");
        assert!(tl[0].time >= tl[1].time, "timeline should be descending");
        assert!(tl[1].time >= tl[2].time, "timeline should be descending");
    }

    /// Objective: Verify that inserting a fact with unknown field recovers.
    /// Invariants: A fact with extra JSON fields is stored and retrieved intact.
    #[test]
    fn fact_with_extra_fields() {
        let store = SqliteFactStore::open_in_memory().unwrap();
        let mut fact = sample_fact(7, FactType::Goal);
        fact.payload = serde_json::json!({
            "goal": "finish LoreScope",
            "deadline": "2026-08",
            "priority": "high",
        });
        store.insert_fact(&fact);

        let facts = store.get_facts(7);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].payload["goal"], "finish LoreScope");
        assert_eq!(facts[0].payload["priority"], "high");
    }
}
