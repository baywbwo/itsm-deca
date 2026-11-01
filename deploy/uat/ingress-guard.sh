#!/bin/sh
# UAT ingress guard. Runs before Caddy starts (and by check-config.sh on the operator host).
# Refuses to start unless BOTH network allowlist and basic auth are configured with safe values.
# Never prints secret values; error messages name the variable only.
#
# Inputs:
#   ITSM_UAT_SITE_ADDRESS      hostname served (no scheme), e.g. itsm-uat.internal.example
#   ITSM_UAT_TLS               "internal" (Caddy local CA) or an ACME account e-mail
#   ITSM_UAT_ALLOWED_CIDRS     space/comma separated IPv4/IPv6 addresses or CIDRs (no 0.0.0.0/0, ::/0)
#   ITSM_UAT_BASIC_AUTH_USER   login name for the ingress gate
#   ITSM_UAT_BASIC_AUTH_HASH_FILE  file holding a bcrypt hash (caddy hash-password), default
#                              /run/secrets/ingress_basic_auth_hash
# Usage: ingress-guard.sh [--check] [command ...]   (--check: validate and exit; else exec command)
set -euf  # -f: no globbing of allowlist tokens such as "*"

fail() { echo "ingress-guard: $*" >&2; exit 64; }

check_only=0
if [ "${1:-}" = "--check" ]; then check_only=1; shift; fi

site=${ITSM_UAT_SITE_ADDRESS:-}
tls=${ITSM_UAT_TLS:-}
cidrs=${ITSM_UAT_ALLOWED_CIDRS:-}
user=${ITSM_UAT_BASIC_AUTH_USER:-}
hash_file=${ITSM_UAT_BASIC_AUTH_HASH_FILE:-/run/secrets/ingress_basic_auth_hash}

[ -n "$site" ] || fail "ITSM_UAT_SITE_ADDRESS belum diisi"
printf '%s' "$site" | grep -Eq '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$' \
  || fail "ITSM_UAT_SITE_ADDRESS harus hostname huruf kecil tanpa skema/port/wildcard"
case "$site" in localhost|*.localhost) fail "ITSM_UAT_SITE_ADDRESS tidak boleh localhost" ;; esac

[ -n "$tls" ] || fail "ITSM_UAT_TLS belum diisi (internal atau e-mail ACME)"
if [ "$tls" != "internal" ]; then
  printf '%s' "$tls" | grep -Eq '^[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}$' \
    || fail "ITSM_UAT_TLS harus 'internal' atau alamat e-mail ACME"
fi

[ -n "$cidrs" ] || fail "ITSM_UAT_ALLOWED_CIDRS belum diisi; akses terbuka tidak diizinkan"
normalized=""
count=0
for c in $(printf '%s' "$cidrs" | tr ',' ' '); do
  count=$((count + 1))
  [ "$count" -le 32 ] || fail "ITSM_UAT_ALLOWED_CIDRS maksimal 32 entri"
  case "$c" in
    */*) addr=${c%/*}; prefix=${c#*/} ;;
    *) addr=$c; prefix="" ;;
  esac
  if printf '%s' "$addr" | grep -Eq '^([0-9]{1,3}\.){3}[0-9]{1,3}$'; then
    for o in $(printf '%s' "$addr" | tr '.' ' '); do
      [ "$o" -le 255 ] 2>/dev/null || fail "ITSM_UAT_ALLOWED_CIDRS berisi IPv4 tidak valid: $c"
    done
    [ "$addr" != "0.0.0.0" ] || fail "ITSM_UAT_ALLOWED_CIDRS tidak boleh 0.0.0.0"
    if [ -n "$prefix" ]; then
      printf '%s' "$prefix" | grep -Eq '^[0-9]{1,2}$' || fail "prefix tidak valid: $c"
      [ "$prefix" -ge 8 ] && [ "$prefix" -le 32 ] || fail "prefix IPv4 harus /8–/32 (terlalu lebar: $c)"
    fi
  elif printf '%s' "$addr" | grep -Eq '^[0-9A-Fa-f:]+$' && printf '%s' "$addr" | grep -q ':'; then
    case "$addr" in ::|::0|0::0) fail "ITSM_UAT_ALLOWED_CIDRS tidak boleh :: (semua alamat)" ;; esac
    if [ -n "$prefix" ]; then
      printf '%s' "$prefix" | grep -Eq '^[0-9]{1,3}$' || fail "prefix tidak valid: $c"
      [ "$prefix" -ge 32 ] && [ "$prefix" -le 128 ] || fail "prefix IPv6 harus /32–/128 (terlalu lebar: $c)"
    fi
  else
    fail "ITSM_UAT_ALLOWED_CIDRS hanya menerima IP/CIDR (bukan nama/keyword): $c"
  fi
  normalized="$normalized $c"
done
[ "$count" -ge 1 ] || fail "ITSM_UAT_ALLOWED_CIDRS kosong"

[ -n "$user" ] || fail "ITSM_UAT_BASIC_AUTH_USER belum diisi"
printf '%s' "$user" | grep -Eq '^[A-Za-z0-9._-]{3,64}$' || fail "ITSM_UAT_BASIC_AUTH_USER: 3–64 karakter [A-Za-z0-9._-]"

[ -r "$hash_file" ] || fail "file hash basic auth tidak ada/terbaca (ITSM_UAT_BASIC_AUTH_HASH_FILE)"
hash=$(tr -d '\r\n' < "$hash_file")
printf '%s' "$hash" | grep -Eq '^\$2[aby]\$(1[0-9]|2[0-9]|3[01])\$[./A-Za-z0-9]{53}$' \
  || fail "hash basic auth harus bcrypt cost >= 10 dari 'caddy hash-password' (bukan password polos)"

if [ "$check_only" -eq 1 ]; then
  echo "ingress-guard: OK (allowlist ${count} entri, basic auth aktif, TLS dikonfigurasi)"
  exit 0
fi

export ITSM_UAT_ALLOWED_CIDRS="${normalized# }"
export ITSM_UAT_BASIC_AUTH_HASH="$hash"
[ "$#" -gt 0 ] || set -- caddy run --config /etc/caddy/Caddyfile --adapter caddyfile
exec "$@"
