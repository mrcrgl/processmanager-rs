#!/usr/bin/env bash
set -euo pipefail

cargo fmt --all -- --check
cargo clippy --all-features -- -D warnings
cargo test --all
cargo test --no-default-features

# Keep examples under CI-appropriate upper bounds.
timeout 30s cargo run --example simple
timeout 45s cargo run --example dynamic_add
timeout 30s cargo run --example restart_supervisor
timeout 30s cargo run --example runtime_context
timeout 30s cargo run --example axum
