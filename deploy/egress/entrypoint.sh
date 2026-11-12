#!/bin/sh
# ITSM egress proxy entrypoint (CP6b): validates the runtime settings, writes
# /run/squid/runtime.conf and runs Squid in the foreground. Never prints secret values.
set -eu
fail() { echo "egress-proxy: $*" >&2; exit 64; }

run_dir=${ITSM_EGRESS_RUN_DIR:-/run/squid}
htpasswd=${ITSM_EGRESS_HTPASSWD_FILE:-/run/secrets/egress_proxy_htpasswd}
cidrs=${ITSM_EGRESS_CLIENT_CIDRS:-172.30.57.0/24}
dns=${ITSM_EGRESS_DNS:-1.1.1.1 8.8.8.8}

[ -s "$htpasswd" ] || fail "secret egress_proxy_htpasswd kosong atau tidak ada"
grep -Evq '^[A-Za-z0-9._-]{1,64}:\$(5|6|apr1|1)\$' "$htpasswd" \
  && fail "egress_proxy_htpasswd harus baris user:hash (openssl passwd -6)"

for c in $cidrs; do
  printf '%s' "$c" | grep -Eq '^([0-9]{1,3}\.){3}[0-9]{1,3}/([0-9]|[12][0-9]|3[0-2])$' \
    || fail "ITSM_EGRESS_CLIENT_CIDRS harus CIDR IPv4 dipisah spasi"
done
for d in $dns; do
  printf '%s' "$d" | grep -Eq '^([0-9]{1,3}\.){3}[0-9]{1,3}$' \
    || fail "ITSM_EGRESS_DNS harus alamat IPv4 resolver dipisah spasi"
done

umask 077
{
  echo "acl itsm_clients src $cidrs"
  echo "dns_nameservers $dns"
} > "$run_dir/runtime.conf"

exec "${ITSM_EGRESS_SQUID:-squid}" -N -f "${ITSM_EGRESS_CONF:-/etc/squid/itsm/squid.conf}" "$@"
