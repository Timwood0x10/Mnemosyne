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

/// A single evidence record (to be inserted in batch).
#[derive(Debug, Clone)]
pub struct EvidenceBatch {
    pub doc_id: i64,
    pub chapter_id: i64,
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
    pub fn new(batch_size: usize) -> Self {
        EvidenceWriter { batch_size }
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
    /// Number of rows successfully inserted.
    pub fn write(&self, conn: &Connection, records: &[EvidenceBatch]) -> usize {
        if records.is_empty() {
            return 0;
        }

        // Use a transaction so the cost of the BEGIN / COMMIT is amortised
        // over all rows instead of being paid once per row.
        conn.execute_batch("BEGIN TRANSACTION;").ok();

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
                values.push(format!(
                    "({}, {}, '{}', strftime('%s','now'))",
                    row.doc_id, row.chapter_id, escaped
                ));
            }
            let sql = format!(
                "INSERT INTO evidence (doc_id, chapter_id, content, created_at) VALUES {}",
                values.join(",")
            );
            if conn.execute(&sql, []).is_ok() {
                written += chunk.len();
            }
        }

        conn.execute_batch("COMMIT;").ok();
        written
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
        let n = writer.write(&conn, &[]);
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
                content: "Chapter 1 text.".into(),
            },
            EvidenceBatch {
                doc_id: 1,
                chapter_id: 2,
                content: "Chapter 2 text.".into(),
            },
            EvidenceBatch {
                doc_id: 1,
                chapter_id: 3,
                content: "Chapter 3 text.".into(),
            },
        ];
        let writer = EvidenceWriter::new(10);
        let n = writer.write(&conn, &records);
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
            content: "Prince O'Brien's regiment".into(),
        }];
        let writer = EvidenceWriter::new(10);
        writer.write(&conn, &records);

        let content: String = conn
            .query_row("SELECT content FROM evidence WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(content, "Prince O'Brien's regiment");
    }
}
