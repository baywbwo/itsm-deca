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
for s in pg_password ingress_basic_auth_hash itsm_keyring egress_proxy_auth egress_proxy_htpasswd; do
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

# Company SSO (CP5c): needs ITSM_ACCESS_MODE=password, which this UAT stack never runs (it is
# uat_superadmin), so sso/both is refused here instead of being silently ignored.
case "${ITSM_AUTH:-}" in
  ''|password) ;;
  sso|both) fail "ITSM_AUTH=$ITSM_AUTH butuh ITSM_ACCESS_MODE=password; compose UAT memakai uat_superadmin (lihat deploy/RUNBOOK-sso.md)" ;;
  *) fail "ITSM_AUTH harus password, sso atau both" ;;
esac
case "${ITSM_OIDC_LAB:-}" in ''|on|off) ;; *) fail "ITSM_OIDC_LAB harus on atau off" ;; esac
if [ -n "${ITSM_PUBLIC_URL:-}" ]; then
  printf '%s' "$ITSM_PUBLIC_URL" | grep -Eq '^https://[a-z0-9]([a-z0-9.-]*[a-z0-9])?(:[0-9]{1,5})?/?$' \
    || fail "ITSM_PUBLIC_URL harus origin https (mis. https://itsm.example.co.id), tanpa path"
fi

# Egress proxy (CP6b): api credentials (user:password) and the proxy password file (user:hash)
# must belong together. Values are compared in pipes only, never printed.
auth=$here/secrets/egress_proxy_auth; htp=$here/secrets/egress_proxy_htpasswd
tr -d '\r\n' < "$auth" | grep -Eq '^[A-Za-z0-9._-]{1,64}:[!-~]{16,256}$' \
  || fail "secrets/egress_proxy_auth harus user:password (password 16–256 karakter tanpa spasi; lihat deploy/RUNBOOK-egress.md)"
[ -s "$htp" ] && ! grep -Evq '^[A-Za-z0-9._-]{1,64}:\$(5|6|apr1|1)\$' "$htp" \
  || fail "secrets/egress_proxy_htpasswd harus baris user:hash (openssl passwd -6)"
proxy_user=$(tr -d '\r\n' < "$auth" | cut -d: -f1)
hash=$(awk -F: -v u="$proxy_user" '$1 == u { sub(/^[^:]*:/, ""); print; exit }' "$htp")
[ -n "$hash" ] || fail "user di secrets/egress_proxy_auth tidak ada di secrets/egress_proxy_htpasswd"
if command -v openssl >/dev/null 2>&1; then
  alg=$(printf '%s' "$hash" | cut -d'$' -f2); salt=$(printf '%s' "$hash" | cut -d'$' -f3)
  again=$(tr -d '\r\n' < "$auth" | cut -d: -f2- | openssl passwd "-$alg" -salt "$salt" -stdin 2>/dev/null) || again=
  [ "$again" = "$hash" ] || fail "password di secrets/egress_proxy_auth tidak cocok dengan hash di secrets/egress_proxy_htpasswd (buat ulang keduanya, lihat deploy/RUNBOOK-egress.md)"
  unset again alg salt
fi
unset hash proxy_user
for d in ${ITSM_EGRESS_DNS:-}; do
  printf '%s' "$d" | grep -Eq '^([0-9]{1,3}\.){3}[0-9]{1,3}$' || fail "ITSM_EGRESS_DNS harus alamat IPv4 resolver dipisah spasi"
done

ITSM_UAT_BASIC_AUTH_HASH_FILE=$here/secrets/ingress_basic_auth_hash "$here/ingress-guard.sh" --check
echo "check-config: OK"
