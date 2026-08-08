.PHONY: all build dist dev check test fmt clean run

SCCACHE := $(shell command -v sccache 2>/dev/null)
CARGO_CACHE := $(if $(SCCACHE),RUSTC_WRAPPER=$(SCCACHE),)

all: build

# Balanced release build with remote embedding support.
build:
	$(CARGO_CACHE) cargo build --release --bin mnemosyne

# Smallest offline distribution binary. Fat LTO trades build time for size.
dist:
	$(CARGO_CACHE) cargo build --profile dist --no-default-features --bin mnemosyne

# Fast incremental development build.
dev:
	$(CARGO_CACHE) cargo build --bin mnemosyne

# Lint and compile every supported feature combination.
check:
	$(CARGO_CACHE) cargo clippy --all-targets --all-features
	$(CARGO_CACHE) cargo check --all-targets --all-features

# Run tests (requires cargo-nextest: cargo install cargo-nextest).
#
# Uses the default feature set (not --all-features): the local-embed ONNX
# stack (fastembed) and the HTTP server integration tests are intentionally
# excluded from the fast inner loop. The HTTP/SSE tests are marked #[ignore]
# and can be run explicitly with:
#   cargo nextest run --run-ignored all
test:
	$(CARGO_CACHE) cargo nextest run

# Format code.
fmt:
	cargo fmt --all

# Clean artifacts.
clean:
	cargo clean

# Run the MCP server with the default lightweight feature set.
run:
	$(CARGO_CACHE) cargo run --bin mnemosyne -- serve
