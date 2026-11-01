#!/bin/sh
# API container entrypoint: builds DATABASE_URL from the Docker secret (never logged),
# migrates, then serves. "seed-uat" is a separate one-off command for the dummy DB only.
set -eu

fail() { echo "api-entrypoint: $*" >&2; exit 64; }

pw_file=${ITSM_DB_PASSWORD_FILE:-/run/secrets/pg_password}
[ -r "$pw_file" ] || fail "secret password DB tidak ada (ITSM_DB_PASSWORD_FILE)"
pw=$(tr -d '\r\n' < "$pw_file")
# Restrict to URL-safe characters so no escaping is needed (generate with: openssl rand -hex 32).
printf '%s' "$pw" | grep -Eq '^[A-Za-z0-9]{24,128}$' || fail "password DB harus 24–128 karakter alfanumerik"
: "${ITSM_DB_HOST:?ITSM_DB_HOST wajib}" "${ITSM_DB_NAME:?ITSM_DB_NAME wajib}" "${ITSM_DB_USER:?ITSM_DB_USER wajib}"
case "$ITSM_DB_NAME" in *dummy*|*uat*) ;; *) fail "ITSM_DB_NAME harus DB UAT dummy (mengandung 'uat' atau 'dummy')" ;; esac
export DATABASE_URL="postgres://${ITSM_DB_USER}:${pw}@${ITSM_DB_HOST}:${ITSM_DB_PORT:-5432}/${ITSM_DB_NAME}"
unset pw

bin=${ITSM_API_BIN:-/usr/local/bin/itsm-api}
cmd=${1:-serve}
case "$cmd" in
  serve)
    "$bin" migrate
    exec "$bin" serve ;;
  migrate)
    exec "$bin" migrate ;;
  seed-uat)
    [ "${ITSM_SEED_CONFIRM_DB:-}" = "$ITSM_DB_NAME" ] || fail "seed-uat butuh ITSM_SEED_CONFIRM_DB=<nama DB dummy>"
    "$bin" migrate
    exec "$bin" seed-uat ;;
  *) fail "perintah tidak dikenal (serve|migrate|seed-uat)" ;;
esac
