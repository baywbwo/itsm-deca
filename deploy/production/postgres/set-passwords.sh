#!/bin/sh
# Sets the itsm_migrator / itsm_app / itsm_backup passwords from their secret files (as the
# superuser over the local socket). Run again after rotating a secret file:
#   docker compose exec -u postgres postgres /usr/local/lib/itsm/set-passwords.sh
# The value goes from the file into psql through a backtick variable, never through argv or the
# environment, and is stored as a SCRAM verifier. Error statements are not logged in this session.
set -eu
psql=${ITSM_PSQL:-psql}
dir=${ITSM_SECRETS_DIR:-/run/secrets}
for pair in itsm_migrator:pg_migrator_password itsm_app:pg_app_password itsm_backup:pg_backup_password; do
  role=${pair%%:*}; file=$dir/${pair#*:}
  [ -r "$file" ] || { echo "set-passwords: secret ${pair#*:} tidak ada" >&2; exit 64; }
  tr -d '\r\n' < "$file" | grep -Eq '^[A-Za-z0-9]{24,128}$' \
    || { echo "set-passwords: ${pair#*:} harus 24–128 karakter alfanumerik (openssl rand -hex 32)" >&2; exit 64; }
  "$psql" -X -q -v ON_ERROR_STOP=1 -d postgres -v role="$role" -v file="$file" <<'SQL'
SET log_min_error_statement = panic;
SET log_statement = 'none';
SET password_encryption = 'scram-sha-256';
\set pw `tr -d '\r\n' < :file`
ALTER ROLE :"role" PASSWORD :'pw';
\unset pw
SQL
done
echo "set-passwords: OK (3 peran)"
