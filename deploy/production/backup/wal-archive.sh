#!/bin/sh
# Postgres archive_command (CP8): wal-archive.sh %p %f
# Encrypts one WAL segment (or .history/.backup file) to the backup volume with the age public
# keys in ITSM_BACKUP_RECIPIENTS. Plaintext never touches the backup volume: age writes a temp
# file that is renamed only once complete. The server holds no private key.
#
# Idempotent the way Postgres requires: if the file was archived before (crash after rename but
# before Postgres recorded it), exit 0 when its plaintext SHA-256 matches, fail when it differs.
set -eu
src=$1; name=$2
dir=${ITSM_BACKUP_DIR:-/backup}
rcpt=${ITSM_BACKUP_RECIPIENTS:-/etc/itsm/backup/age-recipients}
age=${ITSM_AGE_BIN:-age}
case "$name" in */*|.*|'') echo "wal-archive: nama tidak valid" >&2; exit 1 ;; esac
[ -s "$rcpt" ] || { echo "wal-archive: file recipient age tidak ada" >&2; exit 1; }
mkdir -p "$dir/wal" "$dir/wal-sums"
dest=$dir/wal/$name.age
sum=$(sha256sum "$src" | cut -d' ' -f1)
if [ -e "$dest" ]; then
  if [ "$(cat "$dir/wal-sums/$name" 2>/dev/null || true)" = "$sum" ]; then exit 0; fi
  echo "wal-archive: $name sudah ada dengan isi berbeda; tidak ditimpa" >&2
  exit 1
fi
tmp=$dir/wal/.$name.tmp.$$
trap 'rm -f "$tmp" "$dir/wal-sums/.$name.tmp.$$"' EXIT
"$age" -e -R "$rcpt" -o "$tmp" "$src"
printf '%s\n' "$sum" > "$dir/wal-sums/.$name.tmp.$$"
sync "$tmp" "$dir/wal-sums/.$name.tmp.$$" 2>/dev/null || sync
mv "$dir/wal-sums/.$name.tmp.$$" "$dir/wal-sums/$name"
mv "$tmp" "$dest"
trap - EXIT
