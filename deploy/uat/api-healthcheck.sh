#!/bin/bash
# Readiness probe without extra packages: HTTP/1.0 GET over bash /dev/tcp.
set -eu
exec 3<>/dev/tcp/127.0.0.1/8080
printf 'GET /health/ready HTTP/1.0\r\nHost: localhost\r\n\r\n' >&3
IFS= read -r -t 3 status <&3
case "$status" in "HTTP/1."?" 200"*) exit 0 ;; *) exit 1 ;; esac
