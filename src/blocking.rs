//! Blocking-pool bridge shared by the MCP handlers and the storage layer.
//!
//! Every store reaches its data through `rusqlite`, which is synchronous.
//! Running it directly from an `async fn` executes a blocking SQLite call on a
//! tokio *worker* thread, so while one call is in flight the runtime cannot
//! drive its other tasks — a slow query stalls every unrelated connection
//! (audit 09-26/H7).
//!
//! [`run`] is the single hop both layers use: it hands the synchronous body to
//! [`tokio::task::spawn_blocking`] and folds a panicked or cancelled worker
//! back into [`Error::Internal`] so a caller can propagate it with `?` instead
//! of losing the response (audit 09-27/M6).

use crate::error::{Error, Result};

/// Run a synchronous, `Send` closure on tokio's blocking pool.
///
/// The closure owns everything it touches (`move`), so callers clone the
/// `Arc`-held stores and the argument object before calling this — never
/// capture `&self`, which is not `'static`.
///
/// # Errors
///
/// Returns whatever `func` returns. If the blocking task itself cannot run to
/// completion — it panicked or was cancelled during shutdown — that failure is
/// surfaced as [`Error::Internal`] rather than being swallowed, because a lost
/// tool response hangs the client.
///
/// # Examples
///
/// ```
/// # use mnemosyne::error::Result;
/// # async fn demo() -> Result<()> {
/// use mnemosyne::blocking;
///
/// let answer = blocking::run(|| Ok(1 + 1)).await?;
/// assert_eq!(answer, 2);
/// # Ok(())
/// # }
/// ```
pub async fn run<F, T>(func: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(func).await {
        Ok(result) => result,
        // `JoinError` is either a panic in `func` or cancellation while the
        // runtime shuts down; both mean the work produced no answer.
        Err(join_error) => Err(Error::Internal(format!(
            "blocking task failed: {join_error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify the happy path — the closure's value crosses the
    /// blocking-pool boundary unchanged.
    /// Invariants: the returned value equals the closure's return value and
    /// nothing is added or reordered.
    #[tokio::test]
    async fn returns_the_closure_value() {
        let value = run(|| Ok(41 + 1)).await.expect("closure succeeds");
        assert_eq!(value, 42, "value crosses the pool boundary unchanged");
    }

    /// Objective: Verify a closure error is propagated verbatim instead of
    /// being flattened into a join error.
    /// Invariants: the returned error is the closure's own variant/message.
    #[tokio::test]
    async fn propagates_the_closure_error() {
        let err = run(|| Err::<u8, Error>(Error::NotFound("ghost".into())))
            .await
            .expect_err("closure error must surface");
        assert!(
            matches!(&err, Error::NotFound(msg) if msg == "ghost"),
            "closure error variant must survive the hop; got {err:?}"
        );
    }

    /// Objective: Verify the closure really leaves the async worker — it runs
    /// on a different thread than the one awaiting it (the whole point of H7).
    /// Invariants: the closure's thread id differs from the awaiting task's.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn runs_off_the_async_worker() {
        let awaiting = std::thread::current().id();
        let ran_on = run(move || Ok(std::thread::current().id()))
            .await
            .expect("closure succeeds");
        assert_ne!(
            awaiting, ran_on,
            "blocking work must not run on the awaiting worker thread"
        );
    }

    /// Objective: Verify a panicking closure becomes `Error::Internal` (a
    /// structured, actionable failure) instead of unwinding out of `run` and
    /// killing the connection.
    /// Invariants: `run` returns `Err(Error::Internal(..))`; the panic text is
    /// present in the message.
    #[tokio::test]
    async fn panic_becomes_internal_error() {
        let err = run::<_, ()>(|| panic!("boom inside the pool"))
            .await
            .expect_err("a panicking closure must not succeed");
        assert!(
            matches!(&err, Error::Internal(msg) if msg.contains("blocking task failed")),
            "join failure must be folded into Error::Internal; got {err:?}"
        );
    }
}
