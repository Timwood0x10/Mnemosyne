//! Schema definitions for the LoreScope world model (V7).
//!
//! ## Core tables (V7 Entity-centric model)
//!
//! | Table | Purpose |
//! |-------|---------|
//! | `entities` | World entity nodes (person/place/org) |
//! | `entity_profiles` | Entity attributes (字, 籍贯, 外貌, ...) |
//! | `events` | World state changes |
//! | `event_participants` | Who participated in each event |
//! | `relations` | Long-term entity relationships |
//! | `timeline` | Chronological event index |
//!
//! ## Legacy tables (V6 general model, retained for backward compatibility)
//!
//! `documents`, `chapters`, `sentences`, `knowledge_objects`,
//! `knowledge_edges`, `knowledge_evidence`, `evidence`, `mentions`, `compiler_runs`
//!
//! ## Design notes
//!
//! - All surrogate ids are `INTEGER PRIMARY KEY AUTOINCREMENT`.
//! - `created_at` / `updated_at` default to unix seconds.
//! - `entity_id` foreign keys use deferred validation for batch inserts.

/// DDL for the V7 entity-centric world model — executed idempotently by
/// [`crate::knowledge::SQLiteKnowledgeStore::init`].
pub const WORLD_SCHEMA: &str = "
-- ── entities ────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS entities (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL,
    entity_type TEXT NOT NULL DEFAULT 'person',      -- person / place / org / concept
    status      TEXT NOT NULL DEFAULT 'active',       -- active / deceased / disbanded
    importance  REAL DEFAULT 0.5,
    created_at  INTEGER DEFAULT (strftime('%s','localtime')),
    updated_at  INTEGER DEFAULT (strftime('%s','localtime'))
);
CREATE INDEX IF NOT EXISTS idx_entities_name ON entities(name);
CREATE INDEX IF NOT EXISTS idx_entities_type ON entities(entity_type);

-- ── entity_aliases (V7 new) ──────────────────────────────
CREATE TABLE IF NOT EXISTS entity_aliases (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id   INTEGER NOT NULL REFERENCES entities(id),
    alias       TEXT NOT NULL,                       -- courtesy_name / title / nickname
    alias_type  TEXT NOT NULL DEFAULT 'known_as',    -- courtesy / title / name / nickname
    confidence  REAL DEFAULT 1.0,
    UNIQUE(entity_id, alias)
);
CREATE INDEX IF NOT EXISTS idx_aliases_entity ON entity_aliases(entity_id);
CREATE INDEX IF NOT EXISTS idx_aliases_alias ON entity_aliases(alias);

-- ── entity_profiles ─────────────────────────────────────────
CREATE TABLE IF NOT EXISTS entity_profiles (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id   INTEGER NOT NULL REFERENCES entities(id),
    key         TEXT NOT NULL,                   -- courtesy_name / birthplace / appearance / occupation
    value       TEXT NOT NULL,
    confidence  REAL DEFAULT 1.0,
    evidence_id INTEGER REFERENCES evidence(id),
    UNIQUE(entity_id, key)
);
CREATE INDEX IF NOT EXISTS idx_profiles_entity ON entity_profiles(entity_id);

-- ── events ──────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    title       TEXT NOT NULL,                   -- 赤壁之战 / 桃园三结义
    event_type  TEXT NOT NULL DEFAULT 'event',   -- battle / dialogue / death / marriage
    timestamp   INTEGER,                         -- chapter number or year
    location    TEXT,
    description TEXT,
    importance  REAL DEFAULT 0.5,
    created_at  INTEGER DEFAULT (strftime('%s','localtime'))
);
CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);

-- ── event_participants ──────────────────────────────────────
CREATE TABLE IF NOT EXISTS event_participants (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id    INTEGER NOT NULL REFERENCES events(id),
    entity_id   INTEGER NOT NULL REFERENCES entities(id),
    role        TEXT DEFAULT 'participant',      -- protagonist / antagonist / witness
    side        TEXT,                            -- faction / alignment
    UNIQUE(event_id, entity_id)
);
CREATE INDEX IF NOT EXISTS idx_participants_event ON event_participants(event_id);
CREATE INDEX IF NOT EXISTS idx_participants_entity ON event_participants(entity_id);

-- ── relations ───────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS relations (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id       INTEGER NOT NULL REFERENCES entities(id),
    target_id       INTEGER NOT NULL REFERENCES entities(id),
    relation_type   TEXT NOT NULL,               -- brother / enemy / teacher / spouse
    valid_from      INTEGER,                     -- event id where relation started
    valid_to        INTEGER,                     -- event id where relation ended (NULL=ongoing)
    confidence      REAL DEFAULT 1.0,
    created_at      INTEGER DEFAULT (strftime('%s','localtime')),
    UNIQUE(source_id, target_id, relation_type)
);
CREATE INDEX IF NOT EXISTS idx_relations_source ON relations(source_id);
CREATE INDEX IF NOT EXISTS idx_relations_target ON relations(target_id);
";

/// Legacy V6 DDL (retained for backward compatibility).
/// Used by the existing `SQLiteKnowledgeStore` for querying migrated data.
pub const KNOWLEDGE_SCHEMA: &str = "
-- ── documents ───────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS documents (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    title       TEXT NOT NULL,
    author      TEXT,
    doc_type    TEXT,
    created_at  INTEGER DEFAULT (strftime('%s','localtime'))
);
CREATE INDEX IF NOT EXISTS idx_documents_title ON documents(title);

-- ── chapters ─────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS chapters (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    doc_id        INTEGER NOT NULL,
    chapter_no    INTEGER NOT NULL,
    title         TEXT,
    content       TEXT,
    start_offset  INTEGER,
    end_offset    INTEGER,
    FOREIGN KEY(doc_id) REFERENCES documents(id)
);
CREATE INDEX IF NOT EXISTS idx_chapters_doc ON chapters(doc_id);
CREATE INDEX IF NOT EXISTS idx_chapters_doc_no ON chapters(doc_id, chapter_no);

-- ── knowledge_objects ───────────────────────────────────────
-- No domain sub-tables: ALL extra attributes live in `properties` JSON.
CREATE TABLE IF NOT EXISTS knowledge_objects (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    doc_id      INTEGER NOT NULL,
    object_type TEXT NOT NULL,                  -- person / event / place / organization / concept / artifact / role
    name        TEXT NOT NULL,
    properties  JSON DEFAULT '{}',              -- aliases, faction, appearance, personality...
    confidence  REAL DEFAULT 1.0,
    created_at   INTEGER DEFAULT (strftime('%s','localtime')),
    FOREIGN KEY(doc_id) REFERENCES documents(id)
);
CREATE INDEX IF NOT EXISTS idx_ko_name ON knowledge_objects(name);
CREATE INDEX IF NOT EXISTS idx_ko_doc ON knowledge_objects(doc_id);
CREATE INDEX IF NOT EXISTS idx_ko_type ON knowledge_objects(object_type);
CREATE INDEX IF NOT EXISTS idx_ko_doc_name ON knowledge_objects(doc_id, name);

-- ── knowledge_edges ─────────────────────────────────────────
-- Temporal is a key design decision: relations change over the narrative
-- (吕布→丁原: serves ch1 → kills ch3). valid_from/valid_to are chapter_no.
CREATE TABLE IF NOT EXISTS knowledge_edges (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id   INTEGER NOT NULL,
    target_id   INTEGER NOT NULL,
    predicate   TEXT NOT NULL,                  -- serves / kills / trusts / participated_in / located_in
    properties  JSON DEFAULT '{}',              -- weight, confidence, dimension scores...
    origin      TEXT CHECK(origin IN ('observed','derived')) DEFAULT 'observed',
    confidence  REAL DEFAULT 1.0,
    valid_from  INTEGER,                        -- relation start (chapter_no)
    valid_to    INTEGER,                        -- relation end (NULL = ongoing)
    created_at  INTEGER DEFAULT (strftime('%s','localtime')),
    FOREIGN KEY(source_id) REFERENCES knowledge_objects(id),
    FOREIGN KEY(target_id) REFERENCES knowledge_objects(id)
);
CREATE INDEX IF NOT EXISTS idx_ke_source ON knowledge_edges(source_id);
CREATE INDEX IF NOT EXISTS idx_ke_target ON knowledge_edges(target_id);
CREATE INDEX IF NOT EXISTS idx_ke_predicate ON knowledge_edges(predicate);
CREATE INDEX IF NOT EXISTS idx_ke_source_target ON knowledge_edges(source_id, target_id);

-- ── evidence ────────────────────────────────────────────────
-- Evidence does not know who it serves; the m:n link table below associates it.
CREATE TABLE IF NOT EXISTS evidence (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    doc_id        INTEGER NOT NULL,
    chapter_id    INTEGER NOT NULL,
    start_offset  INTEGER,
    end_offset    INTEGER,
    content       TEXT,                          -- original-text snippet
    created_at    INTEGER DEFAULT (strftime('%s','localtime')),
    FOREIGN KEY(doc_id) REFERENCES documents(id),
    FOREIGN KEY(chapter_id) REFERENCES chapters(id)
);
CREATE INDEX IF NOT EXISTS idx_evidence_chapter ON evidence(chapter_id);
CREATE INDEX IF NOT EXISTS idx_evidence_doc ON evidence(doc_id);

-- ── knowledge_evidence (m:n link) ───────────────────────────
-- One sentence can back multiple facts; the UNIQUE constraint prevents
-- duplicating the same (fact, evidence) pair.
CREATE TABLE IF NOT EXISTS knowledge_evidence (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    source_type  TEXT NOT NULL,                 -- 'object' / 'edge'
    source_id    INTEGER NOT NULL,              -- knowledge_objects.id / knowledge_edges.id
    evidence_id  INTEGER NOT NULL,
    FOREIGN KEY(evidence_id) REFERENCES evidence(id),
    UNIQUE(source_type, source_id, evidence_id)
);
CREATE INDEX IF NOT EXISTS idx_kev_source ON knowledge_evidence(source_type, source_id);
CREATE INDEX IF NOT EXISTS idx_kev_evidence ON knowledge_evidence(evidence_id);

-- ── mentions ───────────────────────────────────────────────
-- Entity-resolution index: where each object appears in the text.
CREATE TABLE IF NOT EXISTS mentions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    object_id     INTEGER NOT NULL,
    chapter_id    INTEGER NOT NULL,
    start_offset  INTEGER,
    end_offset    INTEGER,
    alias_used    TEXT,
    confidence    REAL DEFAULT 1.0,
    FOREIGN KEY(object_id) REFERENCES knowledge_objects(id),
    FOREIGN KEY(chapter_id) REFERENCES chapters(id)
);
CREATE INDEX IF NOT EXISTS idx_mentions_object ON mentions(object_id);
CREATE INDEX IF NOT EXISTS idx_mentions_chapter ON mentions(chapter_id);

-- ── compiler_runs ──────────────────────────────────────────
-- Build records for the lore compiler; versions are comparable for
-- coverage/accuracy tracking across releases.
CREATE TABLE IF NOT EXISTS compiler_runs (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    doc_id       INTEGER NOT NULL,
    version      TEXT NOT NULL,                 -- lore-compiler v0.1.0
    started_at   INTEGER,
    finished_at  INTEGER,
    status       TEXT,                          -- running / completed / failed
    statistics   JSON,                          -- objects_count, edges_count, evidence_count
    FOREIGN KEY(doc_id) REFERENCES documents(id)
);
CREATE INDEX IF NOT EXISTS idx_runs_doc ON compiler_runs(doc_id);
";

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Objective: Verify the schema DDL is idempotent — running it twice must
    /// not error (the migrator and store both rely on re-init being safe).
    /// Invariants: Two consecutive executions of `KNOWLEDGE_SCHEMA` succeed;
    /// all eight required tables exist afterwards.
    #[test]
    fn schema_is_idempotent_and_creates_all_tables() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(KNOWLEDGE_SCHEMA)
            .expect("first schema init must succeed");
        // Second run must be a no-op (CREATE TABLE IF NOT EXISTS).
        conn.execute_batch(KNOWLEDGE_SCHEMA)
            .expect("second schema init must be idempotent");

        let required = [
            "documents",
            "chapters",
            "knowledge_objects",
            "knowledge_edges",
            "knowledge_evidence",
            "evidence",
            "mentions",
            "compiler_runs",
        ];
        for table in &required {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    rusqlite::params![table],
                    |row| row.get(0),
                )
                .unwrap_or_else(|e| panic!("query existence of {table}: {e}"));
            assert_eq!(exists, 1, "table `{table}` must exist after schema init");
        }
    }

    /// Objective: Verify the `knowledge_evidence` UNIQUE constraint rejects
    /// duplicate (source_type, source_id, evidence_id) links.
    /// Invariants: Inserting the same triple twice raises a constraint error.
    #[test]
    fn knowledge_evidence_unique_constraint_enforced() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(KNOWLEDGE_SCHEMA).expect("init schema");
        // Minimal parent rows so the FK on evidence_id is satisfiable.
        conn.execute(
            "INSERT INTO documents (title) VALUES ('t')",
            rusqlite::params![],
        )
        .expect("insert doc");
        conn.execute(
            "INSERT INTO chapters (doc_id, chapter_no) VALUES (1, 1)",
            rusqlite::params![],
        )
        .expect("insert chapter");
        conn.execute(
            "INSERT INTO evidence (doc_id, chapter_id, content) VALUES (1, 1, 'c')",
            rusqlite::params![],
        )
        .expect("insert evidence");
        conn.execute(
            "INSERT INTO knowledge_evidence (source_type, source_id, evidence_id) VALUES ('object', 1, 1)",
            rusqlite::params![],
        )
        .expect("first link insert must succeed");
        let dup = conn.execute(
            "INSERT INTO knowledge_evidence (source_type, source_id, evidence_id) VALUES ('object', 1, 1)",
            rusqlite::params![],
        );
        assert!(
            dup.is_err(),
            "duplicate (source_type, source_id, evidence_id) must be rejected by UNIQUE"
        );
    }

    /// Objective: Verify the `knowledge_edges.origin` CHECK constraint only
    /// accepts the two frozen origin values.
    /// Invariants: 'observed'/'derived' succeed; any other value is rejected.
    #[test]
    fn edge_origin_check_constraint_enforced() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(KNOWLEDGE_SCHEMA).expect("init schema");
        conn.execute(
            "INSERT INTO documents (title) VALUES ('t')",
            rusqlite::params![],
        )
        .expect("insert doc");
        // Two stub objects so source_id/target_id are valid FKs.
        conn.execute(
            "INSERT INTO knowledge_objects (doc_id, object_type, name) VALUES (1, 'person', 'a')",
            rusqlite::params![],
        )
        .expect("insert object a");
        conn.execute(
            "INSERT INTO knowledge_objects (doc_id, object_type, name) VALUES (1, 'person', 'b')",
            rusqlite::params![],
        )
        .expect("insert object b");
        for ok in &["observed", "derived"] {
            conn.execute(
                "INSERT INTO knowledge_edges (source_id, target_id, predicate, origin) VALUES (1, 2, 'trusts', ?1)",
                rusqlite::params![ok],
            )
            .unwrap_or_else(|e| panic!("origin `{ok}` must be accepted: {e}"));
        }
        let bad = conn.execute(
            "INSERT INTO knowledge_edges (source_id, target_id, predicate, origin) VALUES (1, 2, 'trusts', 'guessed')",
            rusqlite::params![],
        );
        assert!(
            bad.is_err(),
            "origin `guessed` must be rejected by the CHECK constraint"
        );
    }
}
