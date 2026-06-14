#!/usr/bin/env bash
set -euo pipefail

minimum="${YAAML_COVERAGE_MIN:-70}"

if cargo tarpaulin --version >/dev/null 2>&1; then
  exec cargo tarpaulin \
    --workspace \
    --timeout 120 \
    --fail-under "${minimum}" \
    --out Html \
    --output-dir target/coverage
fi

if cargo llvm-cov --version >/dev/null 2>&1; then
  if ! command -v llvm-cov >/dev/null 2>&1 || ! command -v llvm-profdata >/dev/null 2>&1; then
    if command -v brew >/dev/null 2>&1 && brew --prefix llvm >/dev/null 2>&1; then
      llvm_prefix="$(brew --prefix llvm)"
      export LLVM_COV="${LLVM_COV:-${llvm_prefix}/bin/llvm-cov}"
      export LLVM_PROFDATA="${LLVM_PROFDATA:-${llvm_prefix}/bin/llvm-profdata}"
    elif command -v xcrun >/dev/null 2>&1; then
      export LLVM_COV="${LLVM_COV:-$(xcrun --find llvm-cov)}"
      export LLVM_PROFDATA="${LLVM_PROFDATA:-$(xcrun --find llvm-profdata)}"
    fi
  fi
  cargo llvm-cov clean --workspace
  cargo llvm-cov \
    --workspace \
    --html \
    --output-dir target/coverage/html \
    --fail-under-lines "${minimum}"
  cargo llvm-cov report --summary-only
  exit 0
fi

cat >&2 <<EOF
No Rust coverage tool is installed.

Install one of:
  cargo install cargo-tarpaulin
  cargo install cargo-llvm-cov

Then rerun:
  scripts/check-coverage.sh
EOF
exit 127
