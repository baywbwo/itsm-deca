#!/bin/sh
# Healthy while the newest base backup is younger than interval + 2 hours.
dir=${ITSM_BACKUP_DIR:-/backup}
max=$(( ${ITSM_BACKUP_INTERVAL_SECS:-86400} + 7200 ))
newest=$(ls -1 "$dir/base" 2>/dev/null | grep -E '^[0-9]{8}T[0-9]{6}Z\.meta$' | sort | tail -1)
[ -n "$newest" ] || exit 1
[ $(( $(date +%s) - $(stat -c %Y "$dir/base/$newest") )) -le "$max" ]
