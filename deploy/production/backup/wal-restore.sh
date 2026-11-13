#!/bin/sh
# Postgres restore_command during recovery (CP8): wal-restore.sh %f %p
# Exit 1 = file not in the archive (normal end of archive). A file that exists but does not
# decrypt or does not match its recorded SHA-256 is damage or the wrong key: exit 200, which
# Postgres treats as fatal, so recovery stops instead of silently ending early.
set -eu
name=$1; dest=$2
dir=${ITSM_BACKUP_DIR:-/backup}
id=${ITSM_BACKUP_IDENTITY:?ITSM_BACKUP_IDENTITY wajib (file kunci privat age, hanya saat restore)}
age=${ITSM_AGE_BIN:-age}
src=$dir/wal/$name.age
[ -f "$src" ] || exit 1
tmp=$dest.itsm-tmp
if ! "$age" -d -i "$id" -o "$tmp" "$src"; then
  rm -f "$tmp"; echo "wal-restore: $name gagal didekripsi (kunci salah atau file rusak)" >&2; exit 200
fi
if [ -f "$dir/wal-sums/$name" ] && [ "$(sha256sum "$tmp" | cut -d' ' -f1)" != "$(cat "$dir/wal-sums/$name")" ]; then
  rm -f "$tmp"; echo "wal-restore: checksum $name tidak cocok" >&2; exit 200
fi
mv "$tmp" "$dest"
