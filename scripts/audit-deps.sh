#!/usr/bin/env bash
# CP7d (TM-43): dependency audit. Fails on any RustSec advisory for a crate that is built, and on
# npm advisories of severity high or critical in the shipped web dependencies.
#
# Needs cargo-audit (a tool, not a project dependency):
#   cargo install cargo-audit --locked --version 0.23.0
# Both audits fetch their advisory database (RustSec git, npm registry), so they need network.
#
# Ignored advisories are allowed only for crates that are in Cargo.lock but never built: sqlx
# lists its MySQL and SQLite drivers there although this workspace enables only Postgres. The
# script proves that on every run with `cargo tree`, so an ignore stops working the moment such
# a crate becomes part of the build.
set -euo pipefail
cd "$(dirname "$0")/.."

# advisory id -> crate that must stay out of the build graph
IGNORED=(
  "RUSTSEC-2023-0071 rsa"            # Marvin attack; only via sqlx-mysql (not enabled)
  "RUSTSEC-2026-0459 libsqlite3-sys" # C SQLite CVE; only via sqlx-sqlite (not enabled)
)

args=()
for entry in "${IGNORED[@]}"; do
  read -r id crate <<<"$entry"
  # Prints nothing (exit 0) when no build path leads to the crate.
  if [ -n "$(cargo tree --locked -e all --target all -i "$crate" 2>/dev/null)" ]; then
    echo "audit-deps: $crate is now built; $id can no longer be ignored" >&2
    exit 1
  fi
  args+=(--ignore "$id")
done

echo "== cargo audit"
cargo audit --deny warnings "${args[@]}"

echo "== npm audit (web, shipped dependencies)"
(cd web && npm audit --omit=dev --audit-level=high)
echo "== npm audit (web, all dependencies, report only)"
(cd web && npm audit) || true

echo "audit-deps: OK"
