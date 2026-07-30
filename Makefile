.PHONY: all build dist dev check test fmt clean run

SCCACHE := $(shell command -v sccache 2>/dev/null)
CARGO_CACHE := $(if $(SCCACHE),RUSTC_WRAPPER=$(SCCACHE),)

all: build

# Balanced release build with remote embedding support.
build:
	$(CARGO_CACHE) cargo build --release --bin lore-scope

# Smallest offline distribution binary. Fat LTO trades build time for size.
dist:
	$(CARGO_CACHE) cargo build --profile dist --no-default-features --bin lore-scope

# Fast incremental development build.
dev:
	$(CARGO_CACHE) cargo build --bin lore-scope

# Lint and compile every supported feature combination.
check:
	$(CARGO_CACHE) cargo clippy --all-targets --all-features
	$(CARGO_CACHE) cargo check --all-targets --all-features

# Run tests (requires cargo-nextest: cargo install cargo-nextest).
test:
	$(CARGO_CACHE) cargo nextest run --all-features

# Format code.
fmt:
	cargo fmt --all

# Clean artifacts.
clean:
	cargo clean

# Run the MCP server with the default lightweight feature set.
run:
	$(CARGO_CACHE) cargo run --bin lore-scope -- serve
