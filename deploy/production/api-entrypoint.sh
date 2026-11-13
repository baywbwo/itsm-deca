#!/bin/sh
# Production API container entrypoint (CP8). Builds DATABASE_URL from a secret file (never
# logged) and runs one command. Two database roles:
#   serve, worker            itsm_app       DML only; serve/worker never migrate and refuse to
#                                           start while a migration is pending (verify_schema).
#   migrate, init-password,  itsm_migrator  owns the schema; one-off `docker compose run`.
#   reseal
set -eu

fail() { echo "api-entrypoint: $*" >&2; exit 64; }

[ "${ITSM_ENV:-}" = production ] || fail "ITSM_ENV harus production (paket deploy/production)"
cmd=${1:-serve}
case "$cmd" in
  serve|worker)                 user=itsm_app;      pw_file=${ITSM_DB_APP_PASSWORD_FILE:-/run/secrets/pg_app_password} ;;
  migrate|init-password|reseal) user=itsm_migrator; pw_file=${ITSM_DB_MIGRATOR_PASSWORD_FILE:-/run/secrets/pg_migrator_password} ;;
  *) fail "perintah tidak dikenal (serve|worker|migrate|init-password|reseal)" ;;
esac
[ -r "$pw_file" ] || fail "secret password DB untuk $user tidak ada"
pw=$(tr -d '\r\n' < "$pw_file")
# URL-safe characters only, so no escaping is needed (openssl rand -hex 32).
printf '%s' "$pw" | grep -Eq '^[A-Za-z0-9]{24,128}$' || fail "password DB $user harus 24–128 karakter alfanumerik"
: "${ITSM_DB_HOST:?ITSM_DB_HOST wajib}" "${ITSM_DB_NAME:?ITSM_DB_NAME wajib}"
case "$ITSM_DB_NAME" in *dummy*|*uat*) fail "ITSM_DB_NAME produksi tidak boleh DB UAT/dummy" ;; esac
export DATABASE_URL="postgres://${user}:${pw}@${ITSM_DB_HOST}:${ITSM_DB_PORT:-5432}/${ITSM_DB_NAME}"
unset pw

bin=${ITSM_API_BIN:-/usr/local/bin/itsm-api}
case "$cmd" in
  init-password)
    # The administrator password arrives on stdin (docker compose run -T ... < file); the
    # username and the database confirmation come from ITSM_ADMIN_USERNAME / ITSM_INIT_CONFIRM_DB.
    exec "$bin" init-password ;;
  reseal)
    shift
    exec "$bin" reseal "$@" ;;
  *)
    exec "$bin" "$cmd" ;;
esac
