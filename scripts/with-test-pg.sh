#!/usr/bin/env bash
# Runs a command against a throwaway PostgreSQL cluster (dummy data only).
# PG_BIN must point at a PostgreSQL bin dir; the cluster lives in a temp dir and is
# removed on exit. Listens on 127.0.0.1 only, trust auth inside the throwaway cluster.
# PG_TEST_AUTH=scram uses password auth with the CI's dummy password instead, as the CI
# service container does (catches tests that only pass under trust).
set -euo pipefail
: "${PG_BIN:?PG_BIN wajib diisi (folder bin PostgreSQL)}"
DIR=$(mktemp -d "${TMPDIR:-/tmp}/itsm-rust-pg-XXXXXX")
PORT=${PG_PORT:-$(( 40000 + RANDOM % 20000 ))}
cleanup() { "$PG_BIN/pg_ctl" -D "$DIR/data" -m fast stop >/dev/null 2>&1 || true; rm -rf "$DIR"; }
trap cleanup EXIT
case ${PG_TEST_AUTH:-trust} in
  trust) AUTH=(--auth=trust); CRED=itsm_test ;;
  scram)
    printf '%s\n' itsm_test_dummy_only >"$DIR/pwfile"
    AUTH=(--auth=scram-sha-256 --pwfile="$DIR/pwfile"); CRED=itsm_test:itsm_test_dummy_only ;;
  *) echo "PG_TEST_AUTH: trust|scram" >&2; exit 2 ;;
esac
"$PG_BIN/initdb" -D "$DIR/data" -U itsm_test "${AUTH[@]}" -E UTF8 --no-locale >"$DIR/initdb.log" 2>&1
"$PG_BIN/pg_ctl" -D "$DIR/data" -l "$DIR/pg.log" -w \
  -o "-c listen_addresses=127.0.0.1 -p $PORT -k $DIR -c fsync=off -c max_connections=200" start >/dev/null
export DATABASE_URL="postgres://$CRED@127.0.0.1:$PORT/postgres"
"$@"
