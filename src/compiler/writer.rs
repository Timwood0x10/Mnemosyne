//! Writer — persists compile results to SQLite.
//!
//! Separates the storage layer from the compiler. The compiler produces
//! [`CompileResult`]s, and the Writer writes them to the database through
//! dedicated sub-writers:
//!
//! - [`EvidenceWriter`] — batch-inserts evidence records using a single
//!   transaction, eliminating the O(N) per-row INSERT overhead that was the
//!   dominant bottleneck in War-and-Peace-scale novels (~30k rows → 120s).
//!
//! ## Design
//!
//! ```text
//! Compiler → CompileResult
//!                  ↓
//!           WriterPipeline
//!              ↓       ↓         ↓
//!       Evidence  Entity  Event  Relation
//!       Writer    Writer  Writer  Writer
//!              ↓       ↓         ↓
//!           SQLite (transaction)
//! ```

use rusqlite::Connection;

use crate::error::{Error, Result, StorageError};

/// A single evidence record (to be inserted in batch).
#[derive(Debug, Clone)]
pub struct EvidenceBatch {
    pub doc_id: i64,
    pub chapter_id: i64,
    /// Byte start of the snippet in the source document (schema column exists;
    /// omitting it made every batch-written evidence row unlocatable).
    pub start_offset: Option<i64>,
    /// Byte end (exclusive) of the snippet in the source document.
    pub end_offset: Option<i64>,
    pub content: String,
}

/// Batch evidence writer — uses a single transaction for all inserts.
///
/// # Benchmark
///
/// Before (war_mcp.rs, per-row INSERT):
///   ~31k rows → ~120s (3.3M char War and Peace)
///
/// After (EvidenceWriter with transaction):
///   ~31k rows → ~0.3s estimated (1× transaction, 1× INSERT)
pub struct EvidenceWriter {
    batch_size: usize,
}

impl EvidenceWriter {
    /// Create a new evidence writer with the given batch size.
    ///
    /// `batch_size` controls how many rows are grouped into each SQL
    /// INSERT statement. 100–500 is reasonable for most SQLite configs.
    /// A `batch_size` of 0 is clamped to 1 — `records.chunks(0)` would panic,
    /// and an empty batch is a degenerate caller mistake, not a crash.
    pub fn new(batch_size: usize) -> Self {
        EvidenceWriter {
            batch_size: batch_size.max(1),
        }
    }

    /// Write all evidence records in a single transaction.
    ///
    /// # Parameters
    ///
    /// * `conn` — an open SQLite connection (must have the `evidence` table).
    /// * `records` — the evidence records to insert.
    ///
    /// # Returns
    ///
    /// Number of rows successfully inserted, or a storage error.
    ///
    /// # Errors
    ///
    /// Any INSERT failure aborts the whole batch: the transaction is rolled
    /// back (no partial data) and the error propagates to the caller. This is
    /// deliberately strict — a silent `.ok()` skip previously left the table
    /// partially written while still reporting success.
    pub fn write(&self, conn: &Connection, records: &[EvidenceBatch]) -> Result<usize> {
        if records.is_empty() {
            return Ok(0);
        }

        // Use a transaction so the cost of the BEGIN / COMMIT is amortised
        // over all rows instead of being paid once per row.
        conn.execute_batch("BEGIN TRANSACTION;").map_err(|e| {
            Error::Storage(StorageError::Sqlite(format!(
                "begin evidence transaction: {e}"
            )))
        })?;

        let written = self.write_inner(conn, records);
        match written {
            Ok(n) => {
                if let Err(commit_err) = conn.execute_batch("COMMIT;") {
                    // A failed COMMIT leaves the connection inside an open
                    // transaction holding every "written" row — without an
                    // explicit ROLLBACK those rows silently join later
                    // statements. Mirror the inner-error path.
                    let _ = conn.execute_batch("ROLLBACK;");
                    return Err(Error::Storage(StorageError::Sqlite(format!(
                        "commit evidence transaction (rolled back): {commit_err}"
                    ))));
                }
                Ok(n)
            }
            Err(e) => {
                // Roll back so a mid-batch failure never leaves partial rows.
                let _ = conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    /// The batched INSERT core, run between BEGIN and COMMIT.
    fn write_inner(&self, conn: &Connection, records: &[EvidenceBatch]) -> Result<usize> {
        // Build batched INSERT statements. Each batch inserts `batch_size`
        // rows in a single SQL statement. This is ~100× faster than one
        // INSERT per row because:
        //   1. SQLite parses the statement once instead of N times.
        //   2. The WAL checkpoint happens once instead of N times.
        //   3. Rust→SQLite FFI overhead is paid once per batch.
        let mut written = 0usize;
        for chunk in records.chunks(self.batch_size) {
            let mut values: Vec<String> = Vec::with_capacity(chunk.len());
            for row in chunk {
                let escaped = row.content.replace('\'', "''");
                let start = row
                    .start_offset
                    .map_or_else(|| "NULL".to_string(), |v| v.to_string());
                let end = row
                    .end_offset
                    .map_or_else(|| "NULL".to_string(), |v| v.to_string());
                values.push(format!(
                    "({}, {}, {}, {}, '{}', strftime('%s','now'))",
                    row.doc_id, row.chapter_id, start, end, escaped
                ));
            }
            let sql = format!(
                "INSERT INTO evidence (doc_id, chapter_id, start_offset, end_offset, content, created_at) VALUES {}",
                values.join(",")
            );
            conn.execute(&sql, []).map_err(|e| {
                Error::Storage(StorageError::Sqlite(format!("insert evidence batch: {e}")))
            })?;
            written += chunk.len();
        }
        Ok(written)
    }
}

/// Default evidence writer with 200 rows per batch.
impl Default for EvidenceWriter {
    fn default() -> Self {
        EvidenceWriter::new(200)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_evidence_table(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS evidence (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                doc_id      INTEGER NOT NULL,
                chapter_id  INTEGER NOT NULL,
                start_offset INTEGER,
                end_offset   INTEGER,
                content     TEXT NOT NULL,
                created_at  INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );",
        )
        .unwrap();
    }

    /// Objective: Verify that an empty record list produces zero inserts.
    /// Invariants: No rows are added to the evidence table.
    #[test]
    fn empty_records_write_zero() {
        let conn = Connection::open_in_memory().unwrap();
        create_evidence_table(&conn);
        let writer = EvidenceWriter::new(10);
        let n = writer.write(&conn, &[]).expect("empty write succeeds");
        assert_eq!(n, 0, "empty records should insert 0 rows");
    }

    /// Objective: Verify a small batch is correctly inserted and count matches.
    /// Invariants: Exactly 3 rows exist after writing 3 records.
    #[test]
    fn small_batch_inserts_correctly() {
        let conn = Connection::open_in_memory().unwrap();
        create_evidence_table(&conn);

        let records = vec![
            EvidenceBatch {
                doc_id: 1,
                chapter_id: 1,
                start_offset: Some(0),
                end_offset: Some(15),
                content: "Chapter 1 text.".into(),
            },
            EvidenceBatch {
                doc_id: 1,
                chapter_id: 2,
                start_offset: Some(16),
                end_offset: Some(31),
                content: "Chapter 2 text.".into(),
            },
            EvidenceBatch {
                doc_id: 1,
                chapter_id: 3,
                start_offset: Some(32),
                end_offset: Some(47),
                content: "Chapter 3 text.".into(),
            },
        ];
        let writer = EvidenceWriter::new(10);
        let n = writer.write(&conn, &records).expect("write succeeds");
        assert_eq!(n, 3, "should insert exactly 3 rows");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM evidence", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 3, "evidence table should contain 3 rows");
    }

    /// Objective: Verify that content with single quotes (SQL injection-like)
    /// is properly escaped.
    /// Invariants: A row containing "O'Brien" is stored verbatim.
    #[test]
    fn single_quote_in_content() {
        let conn = Connection::open_in_memory().unwrap();
        create_evidence_table(&conn);

        let records = vec![EvidenceBatch {
            doc_id: 1,
            chapter_id: 1,
            start_offset: Some(0),
            end_offset: Some(27),
            content: "Prince O'Brien's regiment".into(),
        }];
        let writer = EvidenceWriter::new(10);
        writer
            .write(&conn, &records)
            .expect("single-quote write succeeds");

        let content: String = conn
            .query_row("SELECT content FROM evidence WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(content, "Prince O'Brien's regiment");
    }

    /// Objective: Verify a mid-batch INSERT failure propagates and rolls back
    /// the whole transaction (no partial rows survive).
    /// Invariants: with batch_size=1 the first chunk inserts a row, then the
    /// second chunk violates UNIQUE(content) → write returns Err and the
    /// previously inserted row is rolled back (table ends empty).
    #[test]
    fn failing_batch_rolls_back_whole_transaction() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE evidence (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                doc_id      INTEGER NOT NULL,
                chapter_id  INTEGER NOT NULL,
                start_offset INTEGER,
                end_offset   INTEGER,
                content     TEXT NOT NULL,
                created_at  INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                UNIQUE(doc_id, chapter_id, content)
            );",
        )
        .unwrap();

        // Identical content → the second chunk violates the UNIQUE constraint.
        let records = vec![
            EvidenceBatch {
                doc_id: 1,
                chapter_id: 1,
                start_offset: None,
                end_offset: None,
                content: "duplicate".into(),
            },
            EvidenceBatch {
                doc_id: 1,
                chapter_id: 1,
                start_offset: None,
                end_offset: None,
                content: "duplicate".into(),
            },
        ];
        // batch_size=1 forces the first row to commit to the transaction
        // before the second chunk fails — proving rollback, not just a
        // failed insert on an empty table.
        let writer = EvidenceWriter::new(1);
        let err = writer
            .write(&conn, &records)
            .expect_err("a failing batch must return Err instead of silently skipping");
        assert!(
            err.to_string().contains("insert evidence batch"),
            "error names the failing insert, got: {err}"
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM evidence", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0, "failed batch must roll back all rows");
    }

    /// Objective: Verify `batch_size = 0` does not panic (`chunks(0)` would)
    /// and still writes every record.
    /// Invariants: writing 3 records with batch_size 0 succeeds with count 3.
    #[test]
    fn zero_batch_size_does_not_panic() {
        let conn = Connection::open_in_memory().unwrap();
        create_evidence_table(&conn);

        let records = vec![
            EvidenceBatch {
                doc_id: 1,
                chapter_id: 1,
                start_offset: None,
                end_offset: None,
                content: "a".into(),
            },
            EvidenceBatch {
                doc_id: 1,
                chapter_id: 2,
                start_offset: None,
                end_offset: None,
                content: "b".into(),
            },
            EvidenceBatch {
                doc_id: 1,
                chapter_id: 3,
                start_offset: None,
                end_offset: None,
                content: "c".into(),
            },
        ];
        let writer = EvidenceWriter::new(0);
        let n = writer.write(&conn, &records).expect("write succeeds");
        assert_eq!(n, 3, "batch_size 0 must be clamped, all rows written");
    }
}
