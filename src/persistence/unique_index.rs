//! Idempotent installation of the unique indexes a store relies on.
//!
//! Every identity the engine deduplicates on is enforced by the DATABASE, not
//! by in-process bookkeeping — two connections (or two processes) can otherwise
//! both pass a check-then-insert. Installing such an index on a database that
//! predates it is where that promise gets dangerous, so this module owns the
//! two things that must happen together:
//!
//! - **a stale definition is not "already installed"** (a name-only check
//!   accepts an index built on different columns, and every `ON CONFLICT (...)`
//!   that targets it then fails at statement level), and
//! - **dependents are re-pointed before duplicates are deleted**, because
//!   foreign keys are ON by the time this runs and a refused delete takes the
//!   rest of the installation down with it.
//!
//! The pieces are shared by the knowledge store (world model identities) and
//! the fact store (evidence anchors) so the repair logic cannot drift between
//! them.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Result, StorageError};

/// A unique index the store installs at `init`, with everything needed to
/// repair a database written before it existed.
pub(crate) struct UniqueIndex {
    /// Index name; also how a stale definition is detected.
    pub(crate) name: &'static str,
    /// Table the index lives on.
    pub(crate) table: &'static str,
    /// The `CREATE UNIQUE INDEX` statement (compared against the stored one).
    pub(crate) create_sql: &'static str,
    /// The identity expressions, spelled exactly as in the index and in every
    /// `ON CONFLICT (...)` clause that targets it.
    pub(crate) identity: &'static [&'static str],
    /// `(table, column)` pairs pointing at this table's rows.
    ///
    /// A duplicate's id is usually referenced by other rows, and `init` turns
    /// foreign keys ON before this runs, so deleting the duplicate outright is
    /// refused (`FOREIGN KEY constraint failed`) — and the create that follows
    /// then fails too, leaving the store unopenable. Dependents are therefore
    /// moved onto the surviving row first.
    pub(crate) referencing: &'static [(&'static str, &'static str)],
}

/// Create a `UNIQUE_INDEXES` entry idempotently, repairing a dirty database.
///
/// Three things this has to get right:
///
/// - **A stale definition is not "already installed".** Comparing index *names*
///   would accept an index created on different columns, and every
///   `ON CONFLICT (...)` statement would then fail at statement level with a
///   message that hides the cause. The stored `sql` is compared instead (SQLite
///   keeps the statement minus `IF NOT EXISTS`), and a mismatch drops and
///   rebuilds it.
/// - **Dependents are re-pointed before duplicates are deleted** (see
///   [`UniqueIndex::referencing`]); otherwise the delete is refused by the
///   foreign keys and the whole of `init` fails.
/// - **The repair is one transaction**, so a failure cannot leave the index
///   half-installed.
///
/// # Errors
///
/// Returns a schema error naming the index when the check, the repair or the
/// create fails — including the case the previous revision swallowed: a dedupe
/// that could not run.
pub(crate) fn ensure_unique_index(conn: &Connection, spec: &UniqueIndex) -> Result<()> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
            params![spec.name],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| StorageError::Schema(format!("check index {}: {e}", spec.name)))?;
    match stored {
        Some(sql) if normalize_sql(&sql) == normalize_sql(spec.create_sql) => return Ok(()),
        Some(_) => {
            tracing::warn!(
                index = spec.name,
                "unique index definition changed; rebuilding it"
            );
            conn.execute_batch(&format!("DROP INDEX {}", spec.name))
                .map_err(|e| StorageError::Schema(format!("drop index {}: {e}", spec.name)))?;
        }
        None => {}
    }
    let removed = repair_duplicates(conn, spec)
        .map_err(|e| StorageError::Schema(format!("dedupe for {}: {e}", spec.name)))?;
    conn.execute_batch(spec.create_sql)
        .map_err(|e| StorageError::Schema(format!("create index {}: {e}", spec.name)))?;
    if removed > 0 {
        tracing::warn!(
            index = spec.name,
            removed,
            "collapsed duplicate rows before creating the unique index"
        );
    }
    Ok(())
}

/// Collapse every identity group to its freshest row, re-pointing dependents.
///
/// The window function builds `old_id -> new_id` from the identity expressions
/// (`MAX(id)` wins, matching the `DO UPDATE` refresh semantics), dependents are
/// moved with `UPDATE OR IGNORE` — the ignore keeps a dependent whose target
/// already holds the same row from violating its own `UNIQUE` — and whatever
/// could not be moved is deleted, because that row is a duplicate of a link the
/// surviving row already has.
///
/// Returns how many rows were removed.
///
/// # Errors
///
/// Returns the SQLite error when the repair cannot be applied; the transaction
/// is rolled back so the database is left exactly as it was.
fn repair_duplicates(conn: &Connection, spec: &UniqueIndex) -> Result<usize> {
    let partition = spec.identity.join(", ");
    let table = spec.table;
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let repair = (|| -> Result<usize> {
        conn.execute_batch(&format!(
            "DROP TABLE IF EXISTS mn_dupe_map; \
             CREATE TEMP TABLE mn_dupe_map AS \
             SELECT id AS old_id, \
                    MAX(id) OVER (PARTITION BY {partition}) AS new_id \
             FROM {table}; \
             DELETE FROM mn_dupe_map WHERE old_id = new_id;"
        ))?;
        for (child_table, child_column) in spec.referencing {
            // A referencing table can belong to a DIFFERENT store that merely
            // shares this file — `ux_evidence_identity` lists the knowledge
            // store's children — and whichever store opens first sees none of
            // the other's tables. There is then no row to move, so skipping the
            // absent table is correct and keeps the repair from failing on a
            // lookup error that would hide the real one.
            if !table_exists(conn, child_table)? {
                continue;
            }
            conn.execute_batch(&format!(
                "UPDATE OR IGNORE {child_table} \
                 SET {child_column} = (SELECT new_id FROM mn_dupe_map \
                                       WHERE old_id = {child_table}.{child_column}) \
                 WHERE {child_column} IN (SELECT old_id FROM mn_dupe_map); \
                 DELETE FROM {child_table} \
                 WHERE {child_column} IN (SELECT old_id FROM mn_dupe_map);"
            ))?;
        }
        let removed = conn.execute(
            &format!("DELETE FROM {table} WHERE id IN (SELECT old_id FROM mn_dupe_map)"),
            [],
        )?;
        conn.execute_batch("DROP TABLE IF EXISTS mn_dupe_map")?;
        Ok(removed)
    })();
    match repair {
        Ok(removed) => {
            conn.execute_batch("COMMIT")?;
            Ok(removed)
        }
        Err(error) => {
            // Best effort: the caller is about to fail `init`, and a rolled
            // back transaction is what keeps the database untouched.
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// Whether `table` exists in the database.
///
/// # Errors
///
/// Returns the SQLite error when `sqlite_master` cannot be read.
fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let found: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |r| r.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// Normalize a `CREATE INDEX` statement for comparison: SQLite echoes the
/// stored statement without `IF NOT EXISTS`, and whitespace differs because the
/// literals in this file are wrapped across lines.
fn normalize_sql(sql: &str) -> String {
    sql.replace("IF NOT EXISTS", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}
