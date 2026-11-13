#!/bin/sh
# Encrypted physical base backup (CP8), run by the backup service as itsm_backup:
#   base-backup.sh            one backup now, then prune
# pg_basebackup streams one tar (data + the WAL needed to make it consistent + backup_manifest)
# straight into age; nothing is written unencrypted. Output on the backup volume:
#   base/<UTC timestamp>.tar.age   ciphertext
#   base/<UTC timestamp>.meta      start WAL file name (for pruning), sizes; no secrets
# Connection: PGHOST/PGPORT/PGUSER (itsm_backup) and PGPASSFILE or ITSM_BACKUP_PASSWORD_FILE.
set -eu
dir=${ITSM_BACKUP_DIR:-/backup}
rcpt=${ITSM_BACKUP_RECIPIENTS:-/etc/itsm/backup/age-recipients}
age=${ITSM_AGE_BIN:-age}
bin=${ITSM_PG_BIN:-}
[ -s "$rcpt" ] || { echo "base-backup: file recipient age tidak ada" >&2; exit 64; }
if [ -z "${PGPASSFILE:-}" ] && [ -n "${ITSM_BACKUP_PASSWORD_FILE:-}" ]; then
  # libpq passfile built in memory-backed tmp, never logged; host:port:db:user:password.
  umask 077
  PGPASSFILE=$(mktemp); export PGPASSFILE
  trap 'rm -f "$PGPASSFILE"' EXIT
  printf '*:*:*:%s:%s\n' "${PGUSER:-itsm_backup}" "$(tr -d '\r\n' < "$ITSM_BACKUP_PASSWORD_FILE")" > "$PGPASSFILE"
fi
mkdir -p "$dir/base"
ts=$(date -u +%Y%m%dT%H%M%SZ)
out=$dir/base/$ts.tar.age
tmp=$dir/base/.$ts.tmp
log=$(mktemp)
set +e
"${bin:+$bin/}pg_basebackup" -U "${PGUSER:-itsm_backup}" -D - -F tar -X fetch -c fast -v -w 2>"$log" \
  | "$age" -e -R "$rcpt" -o "$tmp"
st=$?
set -e
# pipefail is not POSIX: pg_basebackup failure shows as an error line or a missing end message.
if [ "$st" -ne 0 ] || ! grep -q 'base backup completed' "$log"; then
  grep -v -i password "$log" >&2 || true
  rm -f "$tmp" "$log"
  echo "base-backup: GAGAL" >&2
  exit 1
fi
lsn=$(sed -n 's/.*write-ahead log start point: \([0-9A-F]*\/[0-9A-F]*\) on timeline \([0-9]*\).*/\1 \2/p' "$log" | head -1)
rm -f "$log"
set -- $lsn
[ $# -eq 2 ] || { rm -f "$tmp"; echo "base-backup: titik awal WAL tidak terbaca" >&2; exit 1; }
hi=$(printf '%d' "0x${1%/*}"); lo=$(printf '%d' "0x${1#*/}")
# 16 MiB segments: segment number within the 4 GiB "log id" = lo / 2^24.
start_wal=$(printf '%08X%08X%08X' "$2" "$hi" $((lo / 16777216)))
sync "$tmp" 2>/dev/null || sync
mv "$tmp" "$out"
printf 'start_wal=%s\nbytes=%s\ncreated=%s\n' "$start_wal" "$(wc -c < "$out" | tr -d ' ')" "$ts" > "$dir/base/$ts.meta"
echo "base-backup: OK $ts start_wal=$start_wal"
"$(dirname "$0")/prune.sh"
