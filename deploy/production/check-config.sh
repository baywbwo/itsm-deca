#!/bin/sh
# Operator pre-flight for the production stack (CP8). Run before every `docker compose up`:
#   deploy/production/check-config.sh
# Validates .env, secrets/, backup-recipients and tls/ with the same rules the containers
# enforce, without printing any secret value. Exit 64 with one message on the first problem.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
env_file=${ITSM_PROD_ENV_FILE:-$here/.env}
fail() { echo "check-config: $*" >&2; exit 64; }
mode_of() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }

[ -f "$env_file" ] || fail ".env tidak ada (salin env.example ke deploy/production/.env)"
# KEY=VALUE lines read literally (compose .env syntax), never evaluated by the shell.
while IFS= read -r line || [ -n "$line" ]; do
  case "$line" in ''|'#'*) continue ;; esac
  key=${line%%=*}; val=${line#*=}
  printf '%s' "$key" | grep -Eq '^ITSM_[A-Z0-9_]+$' || continue
  export "$key=$val"
done < "$env_file"

[ -n "${ITSM_PROD_IMAGE_TAG:-}" ] || fail "ITSM_PROD_IMAGE_TAG belum diisi (git SHA rilis)"
printf '%s' "$ITSM_PROD_IMAGE_TAG" | grep -Eq '^[0-9a-f]{7,40}$' || fail "ITSM_PROD_IMAGE_TAG harus git SHA"
for k in ITSM_KEYRING ITSM_MFA_RECOVERY_PEPPER DATABASE_URL ITSM_DB_PASSWORD ITSM_EGRESS_PROXY_AUTH; do
  eval "v=\${$k:-}"
  [ -z "$v" ] || fail "$k tidak boleh di .env; secret hanya di secrets/ (RUNBOOK-production.md)"
done
unset v

# secrets/: 0700 directory, files 0444 (bind-mounted with host permissions; containers run as
# uid 999 postgres / 10001 api), never tracked by git.
[ -d "$here/secrets" ] || fail "secrets/ tidak ada"
[ "$(mode_of "$here/secrets")" = 700 ] || fail "secrets/ harus chmod 700 (sekarang $(mode_of "$here/secrets"))"
for s in pg_superuser_password pg_migrator_password pg_app_password pg_backup_password itsm_keyring \
         mfa_recovery_pepper egress_proxy_auth egress_proxy_htpasswd; do
  f=$here/secrets/$s
  [ -f "$f" ] || fail "secrets/$s tidak ada (boleh kosong hanya itsm_keyring dan mfa_recovery_pepper)"
  case "$(mode_of "$f")" in 444|644) ;; *) fail "secrets/$s harus chmod 444 (sekarang $(mode_of "$f"))" ;; esac
done
for s in pg_superuser_password pg_migrator_password pg_app_password pg_backup_password; do
  tr -d '\r\n' < "$here/secrets/$s" | grep -Eq '^[A-Za-z0-9]{24,128}$' \
    || fail "secrets/$s harus 24–128 karakter alfanumerik (openssl rand -hex 32)"
done
# Four different passwords: one leaked role must not unlock another.
dups=$(for s in pg_superuser_password pg_migrator_password pg_app_password pg_backup_password; do
  tr -d '\r\n' < "$here/secrets/$s" | sha256sum | cut -d' ' -f1; done | sort | uniq -d | wc -l)
[ "$dups" = 0 ] || fail "password Postgres harus berbeda per role (superuser/migrator/app/backup)"

if command -v git >/dev/null 2>&1 && git -C "$here" rev-parse --git-dir >/dev/null 2>&1; then
  tracked=$(git -C "$here" ls-files -- .env secrets tls backup-recipients | head -1)
  [ -z "$tracked" ] || fail ".env, secrets/, tls/ atau backup-recipients ter-track di git; hapus dari index"
fi

# Keyring: key_id:base64(32 bytes)[,…] with ITSM_KEYRING_ACTIVE among them (RUNBOOK-keyring.md).
kr=$(tr -d ' \r\n' < "$here/secrets/itsm_keyring")
if [ -n "$kr" ]; then
  printf '%s' "$kr" | tr ',' '\n' | grep -Evq '^[a-z0-9_-]{1,40}:[A-Za-z0-9+/]{43}=$' \
    && fail "secrets/itsm_keyring harus key_id:base64(32 byte)[,key_id:...] (openssl rand -base64 32)"
  [ -n "${ITSM_KEYRING_ACTIVE:-}" ] || fail "ITSM_KEYRING_ACTIVE wajib bila secrets/itsm_keyring berisi"
  printf '%s' "$kr" | tr ',' '\n' | cut -d: -f1 | grep -qx "$ITSM_KEYRING_ACTIVE" \
    || fail "ITSM_KEYRING_ACTIVE tidak ada di secrets/itsm_keyring"
elif [ -n "${ITSM_KEYRING_ACTIVE:-}" ]; then
  fail "ITSM_KEYRING_ACTIVE diisi tetapi secrets/itsm_keyring kosong"
fi
unset kr

case "${ITSM_AUTH:-password}" in password|sso|both) ;; *) fail "ITSM_AUTH harus password, sso atau both" ;; esac
case "${ITSM_MFA:-off}" in
  off) ;;
  on) tr -d '\r\n' < "$here/secrets/mfa_recovery_pepper" | grep -Eq '^[A-Za-z0-9+/]{43,}=*$' \
        || fail "ITSM_MFA=on butuh secrets/mfa_recovery_pepper = base64 dari >= 32 byte acak (openssl rand -base64 32)" ;;
  *) fail "ITSM_MFA harus on atau off" ;;
esac

# Egress proxy (CP6b): api credentials and the proxy password file must belong together.
auth=$here/secrets/egress_proxy_auth; htp=$here/secrets/egress_proxy_htpasswd
tr -d '\r\n' < "$auth" | grep -Eq '^[A-Za-z0-9._-]{1,64}:[!-~]{16,256}$' \
  || fail "secrets/egress_proxy_auth harus user:password (password 16–256 karakter tanpa spasi; RUNBOOK-egress.md)"
[ -s "$htp" ] && ! grep -Evq '^[A-Za-z0-9._-]{1,64}:\$(5|6|apr1|1)\$' "$htp" \
  || fail "secrets/egress_proxy_htpasswd harus baris user:hash (openssl passwd -6)"
proxy_user=$(tr -d '\r\n' < "$auth" | cut -d: -f1)
hash=$(awk -F: -v u="$proxy_user" '$1 == u { sub(/^[^:]*:/, ""); print; exit }' "$htp")
[ -n "$hash" ] || fail "user di secrets/egress_proxy_auth tidak ada di secrets/egress_proxy_htpasswd"
if command -v openssl >/dev/null 2>&1; then
  alg=$(printf '%s' "$hash" | cut -d'$' -f2); salt=$(printf '%s' "$hash" | cut -d'$' -f3)
  again=$(tr -d '\r\n' < "$auth" | cut -d: -f2- | openssl passwd "-$alg" -salt "$salt" -stdin 2>/dev/null) || again=
  [ "$again" = "$hash" ] || fail "password di secrets/egress_proxy_auth tidak cocok dengan hash di secrets/egress_proxy_htpasswd"
  unset again alg salt
fi
unset hash proxy_user
for d in ${ITSM_EGRESS_DNS:-}; do
  printf '%s' "$d" | grep -Eq '^([0-9]{1,3}\.){3}[0-9]{1,3}$' || fail "ITSM_EGRESS_DNS harus alamat IPv4 resolver dipisah spasi"
done

# Backups: age public keys only. A private key here would let anyone with the backup volume
# read every backup.
rc=$here/backup-recipients
[ -f "$rc" ] || fail "backup-recipients tidak ada (age-keygen -y identity > backup-recipients; kunci privat disimpan di luar host)"
grep -q 'AGE-SECRET-KEY' "$rc" && fail "backup-recipients berisi kunci PRIVAT age; hanya kunci publik (age1...)"
grep -Ev '^(#.*)?$' "$rc" | grep -Evq '^age1[02-9ac-hj-np-z]{58}$' \
  && fail "backup-recipients hanya boleh berisi baris kunci publik age1... (dan komentar #)"
[ "$(grep -Ec '^age1' "$rc")" -ge 1 ] || fail "backup-recipients belum berisi kunci publik"
case "${ITSM_BACKUP_INTERVAL_SECS:-86400}" in ''|*[!0-9]*) fail "ITSM_BACKUP_INTERVAL_SECS harus angka detik" ;; esac
[ "${ITSM_BACKUP_INTERVAL_SECS:-86400}" -ge 3600 ] || fail "ITSM_BACKUP_INTERVAL_SECS minimal 3600"
case "${ITSM_BACKUP_KEEP:-7}" in ''|*[!0-9]*|0) fail "ITSM_BACKUP_KEEP harus angka >= 1" ;; esac

for k in ITSM_DB_MAX_CONNECTIONS ITSM_WORKER_DB_MAX_CONNECTIONS; do
  eval "v=\${$k:-1}"
  case "$v" in ''|*[!0-9]*|0) fail "$k harus angka >= 1" ;; esac
done
# 2 API replicas + worker run as itsm_app, whose CONNECTION LIMIT is 60 (db/roles.sql); keep 5
# spare so a replica restarting before its old connections close is not refused.
total=$(( 2 * ${ITSM_DB_MAX_CONNECTIONS:-10} + ${ITSM_WORKER_DB_MAX_CONNECTIONS:-4} ))
[ "$total" -le 55 ] || fail "pool terlalu besar: 2 x ITSM_DB_MAX_CONNECTIONS + ITSM_WORKER_DB_MAX_CONNECTIONS = $total > 55 (itsm_app CONNECTION LIMIT 60)"
unset v

# Ingress (TLS mode, allowlist, hostname) with the same guard the container runs.
ITSM_PROD_TLS_DIR=${ITSM_PROD_TLS_DIR:-$here/tls} "$here/ingress-guard.sh" --check
if [ "${ITSM_PROD_TLS:-}" = file ]; then
  [ "$(mode_of "$here/tls/key.pem")" = 444 ] || [ "$(mode_of "$here/tls/key.pem")" = 440 ] || [ "$(mode_of "$here/tls/key.pem")" = 400 ] \
    || fail "tls/key.pem harus chmod 400/440/444 di dalam tls/ yang 700"
  [ "$(mode_of "$here/tls")" = 700 ] || [ "$(mode_of "$here/tls")" = 750 ] || fail "tls/ harus chmod 700"
  if command -v openssl >/dev/null 2>&1; then
    openssl x509 -in "$here/tls/cert.pem" -noout -checkend 1209600 >/dev/null 2>&1 \
      || fail "tls/cert.pem kedaluwarsa dalam < 14 hari (atau tidak terbaca)"
    openssl x509 -in "$here/tls/cert.pem" -noout -checkhost "$ITSM_PROD_SITE_ADDRESS" 2>/dev/null | grep -q 'does match' \
      || fail "tls/cert.pem tidak berlaku untuk $ITSM_PROD_SITE_ADDRESS"
    [ "$(openssl x509 -in "$here/tls/cert.pem" -noout -pubkey 2>/dev/null | sha256sum)" = \
      "$(openssl pkey -in "$here/tls/key.pem" -pubout 2>/dev/null | sha256sum)" ] \
      || fail "tls/key.pem bukan pasangan tls/cert.pem"
  fi
fi
echo "check-config: OK"
