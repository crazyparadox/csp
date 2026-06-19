# CSP — Coverage Server Protocol. Run `just` to list recipes.

# Show available recipes.
default:
    @just --list

# Build the whole workspace.
build:
    cargo build

# Run all tests.
test:
    cargo test

# Format the code.
fmt:
    cargo fmt

# Check formatting without writing changes.
fmt-check:
    cargo fmt --check

# Lint with clippy (warnings as errors).
lint:
    cargo clippy --all-targets -- -D warnings

# Regenerate the JSON Schemas in spec/schema/ from the csp-core types.
schema:
    cargo run -p csp-core --example gen_schema

# Install the csp-server binary to ~/.cargo/bin (so neumann can spawn it).
install:
    cargo install --path crates/csp-server

# Pre-commit gate: format check, lint, and tests.
check: fmt-check lint test

# Drive the reference server against a project root with the debug client.
cli root:
    cargo run -p csp-cli -- --root "{{root}}"

# Run the debug client against each bundled example.
demo-rust:
    cargo run -p csp-cli -- --root examples/rust-sample

demo-go:
    cargo run -p csp-cli -- --root examples/go-sample

demo-ts:
    cargo run -p csp-cli -- --root examples/ts-sample

# Run all three example demos.
demo: demo-rust demo-go demo-ts

# Remove build artifacts and generated example coverage.
clean:
    cargo clean
    rm -f examples/rust-sample/lcov.info examples/go-sample/coverage.out
    rm -rf examples/rust-sample/target examples/ts-sample/node_modules
