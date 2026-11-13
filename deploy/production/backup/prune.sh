#!/bin/sh
# Retention (CP8): keep the newest ITSM_BACKUP_KEEP base backups (default 7) and the WAL archive
# from the oldest kept one onwards; older WAL cannot be used without a base backup.
# WAL names are compared like pg_archivecleanup does (log id + segment, timeline ignored);
# timeline .history files are always kept.
set -eu
dir=${ITSM_BACKUP_DIR:-/backup}
keep=${ITSM_BACKUP_KEEP:-7}
case "$keep" in ''|*[!0-9]*|0) echo "prune: ITSM_BACKUP_KEEP harus >= 1" >&2; exit 64 ;; esac
metas=$(ls -1 "$dir/base" 2>/dev/null | grep -E '^[0-9]{8}T[0-9]{6}Z\.meta$' | sort -r || true)
n=0; oldest=
for m in $metas; do
  n=$((n + 1)); ts=${m%.meta}
  if [ "$n" -le "$keep" ]; then oldest=$ts; continue; fi
  rm -f "$dir/base/$ts.tar.age" "$dir/base/$ts.meta"
  echo "prune: hapus base $ts"
done
[ -n "$oldest" ] || exit 0
start=$(sed -n 's/^start_wal=\([0-9A-F]\{24\}\)$/\1/p' "$dir/base/$oldest.meta")
[ -n "$start" ] || { echo "prune: meta $oldest tanpa start_wal" >&2; exit 1; }
removed=0
for sub in wal wal-sums; do
  [ -d "$dir/$sub" ] || continue
  for f in $(ls -1 "$dir/$sub" | grep -E '^[0-9A-F]{24}(\.partial|\.[0-9A-F]{8}\.backup)?(\.age)?$' || true); do
    seg=$(printf '%s' "$f" | cut -c9-24)
    if [ "$seg" \< "$(printf '%s' "$start" | cut -c9-24)" ]; then
      rm -f "$dir/$sub/$f"; removed=$((removed + 1))
    fi
  done
done
echo "prune: simpan $n base terbaru (maks $keep), WAL sebelum $start dihapus ($removed file)"
