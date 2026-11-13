#!/bin/sh
# Backup service main loop (CP8): a base backup whenever the newest one is older than
# ITSM_BACKUP_INTERVAL_SECS (default 86400), checked every 10 minutes. The healthcheck
# (backup/healthcheck.sh) turns unhealthy when no recent backup exists.
set -eu
dir=${ITSM_BACKUP_DIR:-/backup}
interval=${ITSM_BACKUP_INTERVAL_SECS:-86400}
here=$(dirname "$0")
while :; do
  newest=$(ls -1 "$dir/base" 2>/dev/null | grep -E '^[0-9]{8}T[0-9]{6}Z\.meta$' | sort | tail -1 || true)
  age=$interval
  if [ -n "$newest" ]; then age=$(( $(date +%s) - $(stat -c %Y "$dir/base/$newest") )); fi
  if [ "$age" -ge "$interval" ]; then
    "$here/base-backup.sh" || echo "backup-loop: base backup gagal; dicoba lagi 10 menit lagi" >&2
  fi
  sleep 600
done
