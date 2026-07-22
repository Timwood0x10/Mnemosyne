.PHONY: all build dev check test fmt clean run

all: build

# Release build (optimized)
build:
	cargo build --release

# Fast dev build
dev:
	cargo build

# Lint + compile check
check:
	cargo clippy --all-targets --all-features
	cargo check --all-targets --all-features

# Run tests (requires cargo-nextest: cargo install cargo-nextest)
test:
	cargo nextest run --all-features

# Format code
fmt:
	cargo fmt --all

# Clean artifacts
clean:
	cargo clean

# Run the MCP server
run:
	cargo run --bin memory-mcp -- serve
