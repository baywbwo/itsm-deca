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
[ -z "${ITSM_KEYRING:-}" ] || fail "ITSM_KEYRING tidak boleh di .env; pindahkan ke secrets/itsm_keyring (CP5B-04)"
for s in pg_password ingress_basic_auth_hash itsm_keyring; do
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

# Keyring: empty = none; otherwise key_id:base64(32 bytes) entries and ITSM_KEYRING_ACTIVE among them.
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

ITSM_UAT_BASIC_AUTH_HASH_FILE=$here/secrets/ingress_basic_auth_hash "$here/ingress-guard.sh" --check
echo "check-config: OK"
