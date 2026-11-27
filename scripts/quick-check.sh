#!/usr/bin/env bash
# The fast checks of .github/workflows/ci.yml that need no test run: formatting, clippy and the
# generated-file checks (codegen, egg, parity goldens). Keep in sync with ci.yml.
set -uo pipefail
cd "$(dirname "$0")/.."
status=0
step() { echo "== $*"; "$@" || status=1; }
step cargo fmt --all --check
step cargo fmt --check --manifest-path fuzz/Cargo.toml
step cargo xtask codegen --check
step cargo xtask egg --check
step cargo xtask golden check
step cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
exit $status
