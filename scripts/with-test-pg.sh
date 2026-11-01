#!/usr/bin/env bash
# Runs a command against a throwaway PostgreSQL cluster (dummy data only).
# PG_BIN must point at a PostgreSQL bin dir; the cluster lives in a temp dir and is
# removed on exit. Listens on 127.0.0.1 only, trust auth inside the throwaway cluster.
set -euo pipefail
: "${PG_BIN:?PG_BIN wajib diisi (folder bin PostgreSQL)}"
DIR=$(mktemp -d "${TMPDIR:-/tmp}/itsm-rust-pg-XXXXXX")
PORT=${PG_PORT:-$(( 40000 + RANDOM % 20000 ))}
cleanup() { "$PG_BIN/pg_ctl" -D "$DIR/data" -m fast stop >/dev/null 2>&1 || true; rm -rf "$DIR"; }
trap cleanup EXIT
"$PG_BIN/initdb" -D "$DIR/data" -U itsm_test --auth=trust -E UTF8 --no-locale >"$DIR/initdb.log" 2>&1
"$PG_BIN/pg_ctl" -D "$DIR/data" -l "$DIR/pg.log" -w \
  -o "-c listen_addresses=127.0.0.1 -p $PORT -k $DIR -c fsync=off -c max_connections=200" start >/dev/null
export DATABASE_URL="postgres://itsm_test@127.0.0.1:$PORT/postgres"
"$@"
