#!/bin/bash
# Worker liveness (CP7b) without extra packages: the worker has no API listener, so read its own
# metrics (ITSM_METRICS_BIND; port ITSM_WORKER_METRICS_PORT, default 9464) over bash /dev/tcp and
# require a recent successful tick.
# Healthy = itsm_outbox_worker_last_tick_timestamp_seconds within ITSM_WORKER_STALE_SECS (120).
set -eu
max_age=${ITSM_WORKER_STALE_SECS:-120}
exec 3<>/dev/tcp/127.0.0.1/"${ITSM_WORKER_METRICS_PORT:-9464}"
printf 'GET /metrics HTTP/1.0\r\nHost: localhost\r\n\r\n' >&3
IFS= read -r -t 3 status <&3
case "$status" in "HTTP/1."?" 200"*) ;; *) exit 1 ;; esac
last=0
while IFS= read -r -t 3 line <&3; do
  case "$line" in
    "itsm_outbox_worker_last_tick_timestamp_seconds "*) last=${line##* }; last=${last%$'\r'} ;;
  esac
done
case "$last" in ''|*[!0-9]*) exit 1 ;; esac
[ "$last" -gt 0 ] || exit 1
[ $(( $(date +%s) - last )) -le "$max_age" ] || exit 1
