//! Schema definitions for the general knowledge model.
//!
//! Holds the frozen DDL for the eight general-model tables defined in
//! `docs/zh/dev_guide.md` (V2.0 冻结版 §3): `documents`, `chapters`,
//! `knowledge_objects`, `knowledge_edges`, `knowledge_evidence`, `evidence`,
//! `mentions`, and `compiler_runs`. The optional `sentences` table is
//! intentionally omitted (dev_guide §3.1 marks it optional).
//!
//! Design notes:
//! - All surrogate ids are `INTEGER PRIMARY KEY AUTOINCREMENT` (i64 in Rust).
//! - `created_at` columns default to unix seconds via `strftime('%s','localtime')`.
//! - `properties` / `statistics` are `JSON DEFAULT '{}'` — arbitrary attribute
//!   bags so the model never grows new columns (dev_guide §3.4 "不再加字段").
//! - `knowledge_edges.origin` is a `CHECK` column driven by [`Origin`].
//! - `knowledge_evidence` has a `UNIQUE(source_type, source_id, evidence_id)`
//!   constraint so one evidence row can back many facts without duplicating
//!   the link (dev_guide §3.7).

/// DDL for the general knowledge model — executed idempotently by
/// [`crate::knowledge::SQLiteKnowledgeStore::init`](super::super::knowledge::SQLiteKnowledgeStore).
///
/// `CREATE TABLE IF NOT EXISTS` makes re-running safe, which the migrator and
/// tests rely on.
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
