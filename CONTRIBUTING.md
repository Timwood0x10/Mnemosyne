# Contributing to Mnemosyne

Thanks for your interest in improving the Mnemosyne memory distillation
engine! This document explains how to get set up, the conventions we follow,
and how to submit changes.

## Getting started

```bash
# Clone and build
git clone https://github.com/Timwood0x10/Mnemosyne
cd Mnemosyne
cargo build

# Run the test suite (unit + doc tests)
cargo test

# Lint and format check (CI runs the same)
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

A `Makefile` wraps the common workflows:

```bash
make test    # cargo test
make check   # clippy + fmt check
make run     # run the stdio MCP server
```

## Development workflow

1. Fork the repository and create a topic branch (`feat/...`, `fix/...`).
2. Make your change with tests. New behaviour needs unit tests; bug fixes
   need a regression test that fails before the fix.
3. Keep commits focused and write clear messages.
4. Open a pull request against `main`. CI must pass (build, test, clippy, fmt).

## Code style

- `cargo fmt` is the formatter; do not reformat unrelated code.
- Prefer explicit error handling via the `Error` type in `src/error.rs`.
- Add module-level and public-item doc comments. Doctests are encouraged.
- When fixing a bug, add a short "Objective / Invariants" doc comment to the
  regression test so the intent is preserved.

## Reporting bugs

Please open a GitHub issue with a minimal reproduction. For security issues,
follow [SECURITY.md](./SECURITY.md) instead of public issues.

## License

By contributing, you agree that your contributions are licensed under the
Apache-2.0 License.
