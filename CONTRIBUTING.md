# Contributing to Cognitive Memory MCP Server

Thanks for your interest in improving this project. This document captures the
local conventions that keep the codebase healthy. Read it once before opening a
PR; the rules below are enforced in review.

## Quick start

```bash
git clone <repo-url>   # or your fork
cd memory_distill

make dev        # fast dev build (cargo build)
make run        # start the stdio MCP server
make test       # requires cargo-nextest: cargo install cargo-nextest
``

If you do not have `cargo-nextest`, `cargo test --all-features` works too —
the Makefile target only exists because nextest is faster locally.

## Development commands

| Command | What it does |
|---------|--------------|
| `make dev` | `cargo build` — fast unoptimized build |
| `make build` | `cargo build --release` — optimized build |
| `make check` | `cargo clippy` + `cargo check`, all targets, all features |
| `make fmt` | `cargo fmt --all` — format the code |
| `make test` | `cargo nextest run --all-features` |
| `make clean` | `cargo clean` |
| `make run` | `cargo run --bin memory-mcp -- serve` |

**Before every commit**, run:

```bash
make fmt && make check && make test
``

Zero errors from `make check` is the hard gate. Warnings from clippy may be
left alone (do not block a PR on them), but never introduce a new warning
without mentioning it in the PR description.

## Branch and PR conventions

- Branch off `main`. Name branches `feat/<topic>`, `fix/<topic>`, or
  `docs/<topic>`.
- One logical change per PR. A bug fix that also refactors surrounding code
  will be sent back — split it.
- PR title follows [Conventional Commits](https://www.conventionalcommits.org/):
  `feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`. Scope optional.
- The PR body must list: what changed, why, how it was tested, and any
  follow-up items left intentionally.
- Do not bump `Cargo.toml` version in a feature PR. The maintainer cuts
  releases.

## Code style rules (enforced)

These rules originate from the project's own `plan/rules/rules.md` and are
non-negotiable for new contributions:

1. **Single file ≤ 1000 lines** (including tests and comments). When a file
   approaches the limit, split it into a submodule under `src/<module>/` with
   a `mod.rs` re-exporting the public surface. Do not pad a file past the
   limit by moving tests into a separate `tests.rs` — that is fine and
   encouraged, but tests still count toward the 1000-line cap of their host
   file.

2. **`make check` must report 0 errors.** Warnings are tolerated but tracked;
   do not let them accumulate.

3. **`cargo fmt --all` after every change.** Unformatted diffs will be
   rejected by review even when the logic is correct.

4. **Never silence warnings with `#[allow(...)]`.** Fix the underlying issue or
   argue in the PR why the warning is spurious. The only exception is
   `#[allow(dead_code)]` on a struct field that is part of a documented
   future API surface — and even then, prefer deleting the field.

5. **Comments are in English.** This applies to both `//` line comments and
   `///`/`//!` doc comments. The codebase has Chinese user-facing strings
   (e.g. `compress_pair` outputs `"问题：解决方案"` format) and Chinese
   Pinyin identifiers are tolerated where they preserve an existing naming
   convention, but new code uses English identifiers.

6. **Error handling: no silent failures.** Every `Result::Err` path must be
   either propagated with `?` or matched explicitly with a traced decision.
   Do not `let _ = something_that_can_fail();`. Use `tracing::warn!` or
   `tracing::error!` if you genuinely must drop an error, and document why.

7. **No `unwrap()` / `expect()` in library code** unless you can prove the
   call cannot fail — and then add a comment stating the proof. Tests may
   use `expect()` freely with a message explaining the invariant.

8. **Public functions carry doc comments.** Every `pub fn`, `pub struct`,
   `pub enum`, and `pub trait` must have a `///` comment describing purpose,
   arguments, return value, and `# Errors` (when applicable). Private items
   only need a comment when the intent is non-obvious.

9. **No magic numbers.** Replace `42` with `const MAX_DEPTH: usize = 42;` or
   equivalent. Named constants make diffs readable across years.

10. **FFI boundaries (when added)** use `extern "C"` + `int` error codes, and
    batch transfers across the boundary rather than calling per-symbol.

## Testing standards

The project follows a "Golden Trio" testing discipline. New tests must:

- Have a `/// Objective:` and `/// Invariants:` doc comment per test, stating
  what the test proves and what invariants hold before/after.
- Use `assert_eq!` / `assert!` **with a context message**, e.g.
  `assert_eq!(v.len(), 3, "vector must have 3 dims after embed, got {v:?}")`.
- Cover the happy path, at least one edge case (empty input, zero, boundary
  value), and — for anything touching `Mutex`/`Atomic`/concurrency — a
  concurrent stress case.
- Never `println!` in tests. Use `tracing`'s test subscriber if logging is
  needed; otherwise rely on the assertion message.

Property-based and fuzz tests live under `tests/` (integration). Unit tests
live in the `#[cfg(test)] mod tests` block at the bottom of each source file.

**Do not run coverage tooling.** It wastes CI minutes and is not part of the
review gate. The gate is `make check` (0 errors) + `make test` (all pass).

## Architecture orientation

New contributors should read, in order:

1. `README.md` — the 8-stage pipeline overview and the Mermaid architecture
   diagram. This is the map.
2. `src/lib.rs` — module index with one-line responsibilities.
3. `src/distiller.rs` — the pipeline orchestrator. Phases are numbered 1–8
   and the `distill()` method is the entry point.
4. `plan/rules/rules.md` — the full coding-standard document. The rules above
   are excerpted from it; the source document is authoritative on conflict.

## Reporting bugs

Open an issue with:
- the exact command that reproduces the failure,
- the `MEMORY_*` environment variables in effect (do NOT paste API keys),
- the `tracing` output (stderr) up to the failure point,
- the SQLite schema version if you can run `sqlite3 memory.db ".schema"`.

Security vulnerabilities (e.g. a way to bypass `SecurityFilter` and persist a
secret) should be reported privately to the maintainer before any public
disclosure.

## License

By submitting a PR, you agree your contributions are licensed under
[Apache-2.0](./LICENSE), the project's license. No CLA is required.
