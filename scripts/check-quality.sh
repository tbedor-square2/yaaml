#!/usr/bin/env bash
set -euo pipefail

cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
scripts/check-coverage.sh
