#!/bin/sh
# Point-in-time restore (CP8) into an EMPTY data directory:
#   ITSM_BACKUP_IDENTITY=/path/age-identity restore.sh <base.tar.age> <data-dir> ["<target time>"]
# Target time like '2026-11-13 08:30:00+00' (default: replay the whole archive). Decrypts the base
# backup into <data-dir>, verifies it against its backup_manifest, and writes the recovery
# settings; then start Postgres on <data-dir> (deploy/RUNBOOK-production.md "Restore"). Recovery
# promotes at the target; WAL that does not decrypt stops recovery (wal-restore.sh exit 200).
# The identity file is needed only for the restore and must not stay on the database host.
set -eu
base=${1:?base backup .tar.age}; data=${2:?direktori data tujuan (kosong)}; target=${3:-}
id=${ITSM_BACKUP_IDENTITY:?ITSM_BACKUP_IDENTITY wajib (file kunci privat age)}
dir=${ITSM_BACKUP_DIR:-/backup}
age=${ITSM_AGE_BIN:-age}
bin=${ITSM_PG_BIN:-}
walrestore=${ITSM_WAL_RESTORE:-/usr/local/lib/itsm/backup/wal-restore.sh}
fail() { echo "restore: $*" >&2; exit 64; }
[ -f "$base" ] || fail "base backup tidak ada"
[ -r "$id" ] || fail "file identity tidak terbaca"
for v in "$id" "$dir" "$walrestore" "$data"; do
  case "$v" in *"'"*|*' '*) fail "path tidak boleh berisi spasi atau kutip" ;; esac
done
if [ -n "$target" ]; then
  printf '%s' "$target" | grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]+)?(\+00|Z| UTC)$' \
    || fail "waktu target harus 'YYYY-MM-DD HH:MM:SS+00' (UTC)"
fi
mkdir -p "$data"
[ -z "$(ls -A "$data")" ] || fail "direktori data tujuan tidak kosong"
chmod 0700 "$data"
# age authenticates every chunk: a wrong key or a modified file fails here, before tar sees data.
bad=$(mktemp)
{ "$age" -d -i "$id" "$base" || echo age >> "$bad"; } | tar -x -C "$data" -f - || echo tar >> "$bad"
if [ -s "$bad" ]; then
  rm -rf "${data:?}"/* "$bad"
  fail "base backup gagal didekripsi/diekstrak (kunci salah atau file rusak); direktori tujuan dikosongkan"
fi
rm -f "$bad"
"${bin:+$bin/}pg_verifybackup" -n "$data" >/dev/null || { rm -rf "${data:?}"/*; fail "base backup tidak cocok dengan backup_manifest"; }
{
  echo "# CP8 restore $(date -u +%Y-%m-%dT%H:%M:%SZ) from $(basename "$base")"
  echo "restore_command = 'ITSM_BACKUP_DIR=$dir ITSM_BACKUP_IDENTITY=$id ITSM_AGE_BIN=$age $walrestore %f %p'"
  if [ -n "$target" ]; then
    echo "recovery_target_time = '$target'"
    echo "recovery_target_inclusive = on"
  fi
  echo "recovery_target_action = 'promote'"
} >> "$data/postgresql.auto.conf"
: > "$data/recovery.signal"
echo "restore: base diekstrak dan terverifikasi; jalankan Postgres pada direktori ini untuk replay WAL${target:+ sampai $target}"
