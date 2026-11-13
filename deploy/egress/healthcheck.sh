#!/bin/bash
# Liveness from inside the container: Squid answers an unauthenticated CONNECT with 407.
set -eu
exec 3<>/dev/tcp/127.0.0.1/3128
printf 'CONNECT health.invalid:443 HTTP/1.1\r\nHost: health.invalid:443\r\n\r\n' >&3
IFS= read -r -t 3 status <&3
case "$status" in "HTTP/1."?" 407"*|"HTTP/1."?" 403"*) exit 0 ;; *) exit 1 ;; esac
