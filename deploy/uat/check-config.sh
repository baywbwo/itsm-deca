#!/bin/sh
# Operator pre-flight for the UAT stack. Run from anywhere: deploy/uat/check-config.sh
# Validates the same rules the containers enforce, without printing secret values.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
env_file=${ITSM_UAT_ENV_FILE:-$here/.env}
fail() { echo "check-config: $*" >&2; exit 64; }

[ -f "$env_file" ] || fail ".env tidak ada (salin env.example ke deploy/uat/.env)"
# Read KEY=VALUE lines literally (compose .env syntax), never via shell evaluation.
while IFS= read -r line || [ -n "$line" ]; do
  case "$line" in ''|'#'*) continue ;; esac
  key=${line%%=*}; val=${line#*=}
  printf '%s' "$key" | grep -Eq '^ITSM_[A-Z0-9_]+$' || continue
  export "$key=$val"
done < "$env_file"

[ -n "${ITSM_UAT_IMAGE_TAG:-}" ] || fail "ITSM_UAT_IMAGE_TAG belum diisi (git SHA kandidat)"
printf '%s' "$ITSM_UAT_IMAGE_TAG" | grep -Eq '^[0-9a-f]{7,40}$' || fail "ITSM_UAT_IMAGE_TAG harus git SHA"

dmode=$(stat -c %a "$here/secrets" 2>/dev/null || stat -f %Lp "$here/secrets")
# Compose (non-swarm) bind-mounts secret files with host permissions: containers run as other
# uids (postgres 999, api 10001), so files must be world-readable INSIDE a 0700 directory.
[ "$dmode" = 700 ] || fail "secrets/ harus chmod 700 (sekarang $dmode)"
for s in pg_password ingress_basic_auth_hash; do
  f=$here/secrets/$s
  [ -f "$f" ] || fail "secrets/$s tidak ada"
  mode=$(stat -c %a "$f" 2>/dev/null || stat -f %Lp "$f")
  case "$mode" in 444|644) ;; *) fail "secrets/$s harus chmod 444 (sekarang $mode); direktori secrets/ yang 700 membatasi akses host" ;; esac
done
tr -d '\r\n' < "$here/secrets/pg_password" | grep -Eq '^[A-Za-z0-9]{24,128}$' \
  || fail "secrets/pg_password harus 24–128 karakter alfanumerik (openssl rand -hex 32)"

if command -v git >/dev/null 2>&1 && git -C "$here" rev-parse --git-dir >/dev/null 2>&1; then
  tracked=$(git -C "$here" ls-files -- .env secrets | head -1)
  [ -z "$tracked" ] || fail ".env atau secrets/ ter-track di git; hapus dari index"
fi

ITSM_UAT_BASIC_AUTH_HASH_FILE=$here/secrets/ingress_basic_auth_hash "$here/ingress-guard.sh" --check
echo "check-config: OK"
