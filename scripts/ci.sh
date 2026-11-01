#!/usr/bin/env bash
# Local gate: fmt, clippy, tests against a throwaway dummy PostgreSQL.
# Usage: PG_BIN=/path/to/pg/bin scripts/ci.sh   (logs under $EVIDENCE_DIR when set)
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/env.sh
out() { if [ -n "${EVIDENCE_DIR:-}" ]; then tee "$EVIDENCE_DIR/$1.log"; else cat; fi; }
echo "sha=$(git rev-parse HEAD) dirty=$(git status --porcelain | wc -l)" | out head
cargo --version | out toolchain; rustc --version | out toolchain-rustc
cargo fmt --all -- --check 2>&1 | out fmt
cargo clippy --locked --all-targets -- -D warnings 2>&1 | out clippy
scripts/with-test-pg.sh cargo test --locked --all-targets 2>&1 | out test
