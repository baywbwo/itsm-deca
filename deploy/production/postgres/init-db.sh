#!/bin/sh
# First start of an empty cluster (official image runs /docker-entrypoint-initdb.d/*.sh as the
# OS user postgres over the local socket). Idempotent, so it can be re-run by hand:
#   docker compose exec -u postgres postgres /usr/local/lib/itsm/init-db.sh
# Creates the three roles, the application database owned by itsm_migrator, the database-level
# privileges, and sets role passwords from the secret files. Tables, and the app's table
# privileges, come later from `migrate` (run as itsm_migrator).
set -eu
psql=${ITSM_PSQL:-psql}
db=${ITSM_DB_NAME:-itsm}
sqldir=${ITSM_DB_SQL_DIR:-/etc/itsm/db}
printf '%s' "$db" | grep -Eq '^[a-z][a-z0-9_]{0,62}$' || { echo "init-db: ITSM_DB_NAME tidak valid" >&2; exit 64; }
run() { "$psql" -X -q -v ON_ERROR_STOP=1 "$@"; }
run -d postgres -f "$sqldir/roles.sql"
if [ "$(run -tA -d postgres -c "SELECT count(*) FROM pg_database WHERE datname = '$db'")" = 0 ]; then
  run -d postgres -c "CREATE DATABASE $db OWNER itsm_migrator ENCODING 'UTF8' TEMPLATE template0"
fi
run -d "$db" -f "$sqldir/database.sql"
# Same database: tables created by a migrate that ran before this script (re-run) get the grants.
if [ "$(run -tA -d "$db" -c "SELECT count(*) FROM pg_proc WHERE proname = 'itsm_grant_app'")" = 1 ]; then
  run -d "$db" -c "SELECT itsm_grant_app()" >/dev/null
fi
"${ITSM_SET_PASSWORDS:-/usr/local/lib/itsm/set-passwords.sh}"
echo "init-db: peran, database $db dan privilege siap"
