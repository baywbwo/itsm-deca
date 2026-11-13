#!/bin/sh
# Production ingress guard (CP8). Runs before Caddy starts (and from check-config.sh).
# Refuses to start without a valid site address, TLS mode and network allowlist.
# Inputs:
#   ITSM_PROD_SITE_ADDRESS   hostname served (no scheme), e.g. itsm.deca.co.id
#   ITSM_PROD_TLS            internal | <ACME e-mail> | file  (file = /etc/itsm/tls/cert.pem +
#                            key.pem from the company CA; ITSM_PROD_TLS_DIR overrides the folder)
#   ITSM_PROD_ALLOWED_CIDRS  space/comma separated IPv4/IPv6 addresses or CIDRs
#   ITSM_PROD_PUBLIC_ACCESS  off (default) | on. on is the only way to allow 0.0.0.0/0 and ::/0
#                            (internet-facing; an owner decision, see RUNBOOK-production.md).
# Usage: ingress-guard.sh [--check] [command ...]
set -euf
fail() { echo "ingress-guard: $*" >&2; exit 64; }
check_only=0
if [ "${1:-}" = "--check" ]; then check_only=1; shift; fi

site=${ITSM_PROD_SITE_ADDRESS:-}
tls=${ITSM_PROD_TLS:-}
cidrs=${ITSM_PROD_ALLOWED_CIDRS:-}
public=${ITSM_PROD_PUBLIC_ACCESS:-off}
tls_dir=${ITSM_PROD_TLS_DIR:-/etc/itsm/tls}

[ -n "$site" ] || fail "ITSM_PROD_SITE_ADDRESS belum diisi"
printf '%s' "$site" | grep -Eq '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$' \
  || fail "ITSM_PROD_SITE_ADDRESS harus hostname huruf kecil tanpa skema/port/wildcard"
case "$site" in localhost|*.localhost) fail "ITSM_PROD_SITE_ADDRESS tidak boleh localhost" ;; esac

case "$tls" in
  '') fail "ITSM_PROD_TLS belum diisi (internal, e-mail ACME, atau file)" ;;
  internal) ;;
  file)
    case "$tls_dir" in *' '*) fail "ITSM_PROD_TLS_DIR tidak boleh berisi spasi" ;; esac
    [ -r "$tls_dir/cert.pem" ] && [ -r "$tls_dir/key.pem" ] || fail "ITSM_PROD_TLS=file butuh cert.pem dan key.pem di $tls_dir"
    grep -q 'BEGIN CERTIFICATE' "$tls_dir/cert.pem" || fail "cert.pem bukan sertifikat PEM"
    tls="$tls_dir/cert.pem $tls_dir/key.pem" ;;
  *) printf '%s' "$tls" | grep -Eq '^[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}$' \
       || fail "ITSM_PROD_TLS harus internal, file, atau alamat e-mail ACME" ;;
esac

case "$public" in on|off) ;; *) fail "ITSM_PROD_PUBLIC_ACCESS harus on atau off" ;; esac
[ -n "$cidrs" ] || fail "ITSM_PROD_ALLOWED_CIDRS belum diisi (jaringan kantor/VPN; akses internet hanya dengan ITSM_PROD_PUBLIC_ACCESS=on)"
normalized=""; count=0
for c in $(printf '%s' "$cidrs" | tr ',' ' '); do
  count=$((count + 1))
  [ "$count" -le 64 ] || fail "ITSM_PROD_ALLOWED_CIDRS maksimal 64 entri"
  case "$c" in
    0.0.0.0/0|::/0)
      [ "$public" = on ] || fail "$c membuka akses ke seluruh internet; hanya dengan ITSM_PROD_PUBLIC_ACCESS=on"
      normalized="$normalized $c"; continue ;;
  esac
  case "$c" in */*) addr=${c%/*}; prefix=${c#*/} ;; *) addr=$c; prefix="" ;; esac
  if printf '%s' "$addr" | grep -Eq '^([0-9]{1,3}\.){3}[0-9]{1,3}$'; then
    for o in $(printf '%s' "$addr" | tr '.' ' '); do
      [ "$o" -le 255 ] 2>/dev/null || fail "IPv4 tidak valid: $c"
    done
    [ "$addr" != "0.0.0.0" ] || fail "0.0.0.0 tidak valid (pakai 0.0.0.0/0 dengan ITSM_PROD_PUBLIC_ACCESS=on)"
    if [ -n "$prefix" ]; then
      printf '%s' "$prefix" | grep -Eq '^[0-9]{1,2}$' || fail "prefix tidak valid: $c"
      [ "$prefix" -ge 8 ] && [ "$prefix" -le 32 ] || fail "prefix IPv4 harus /8–/32 (terlalu lebar: $c)"
    fi
  elif printf '%s' "$addr" | grep -Eq '^[0-9A-Fa-f:]+$' && printf '%s' "$addr" | grep -q ':'; then
    case "$addr" in ::|::0|0::0) fail ":: tidak valid (pakai ::/0 dengan ITSM_PROD_PUBLIC_ACCESS=on)" ;; esac
    if [ -n "$prefix" ]; then
      printf '%s' "$prefix" | grep -Eq '^[0-9]{1,3}$' || fail "prefix tidak valid: $c"
      [ "$prefix" -ge 32 ] && [ "$prefix" -le 128 ] || fail "prefix IPv6 harus /32–/128 (terlalu lebar: $c)"
    fi
  else
    fail "ITSM_PROD_ALLOWED_CIDRS hanya menerima IP/CIDR: $c"
  fi
  normalized="$normalized $c"
done

upstreams=${ITSM_PROD_API_UPSTREAMS:-api-a:8080 api-b:8080}
for u in $upstreams; do
  printf '%s' "$u" | grep -Eq '^[A-Za-z0-9.-]+:[0-9]{1,5}$' || fail "ITSM_PROD_API_UPSTREAMS tidak valid: $u"
done

if [ "$check_only" -eq 1 ]; then
  echo "ingress-guard: OK (allowlist $count entri, akses publik $public, TLS dikonfigurasi)"
  exit 0
fi
export ITSM_PROD_ALLOWED_CIDRS="${normalized# }" ITSM_PROD_TLS="$tls" ITSM_PROD_API_UPSTREAMS="$upstreams"
[ "$#" -gt 0 ] || set -- caddy run --config /etc/caddy/Caddyfile --adapter caddyfile
exec "$@"
