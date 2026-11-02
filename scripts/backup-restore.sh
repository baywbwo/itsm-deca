#!/usr/bin/env bash
# Backup/restore proof on a throwaway cluster, dummy data only (RUNBOOK §6, G15/G25).
# Usage (repo root, debug binary built):
#   PG_BIN=<server bin> PG_CLIENT_BIN=<pg_dump/pg_restore/psql bin> [PG_CLIENT_LIB=<libdir>] \
#   OUT=<evidence dir> scripts/with-test-pg.sh scripts/backup-restore.sh
# Dumps a migrated + seeded source DB with pg_dump -Fc, restores it into a new DB, and checks
# that tables/rows, constraints, indexes, triggers (incl. enabled state), functions and the
# migration ledger match, that constraints still hold, that the append-only triggers (0011
# platform_audit_events and the earlier audit tables) still refuse UPDATE/DELETE/TRUNCATE, and
# that migrate is a no-op on the restored DB.
set -euo pipefail
umask 077
B=${PG_CLIENT_BIN:?PG_CLIENT_BIN wajib diisi (folder pg_dump/pg_restore/psql)}
if [ -n "${PG_CLIENT_LIB:-}" ]; then export LD_LIBRARY_PATH="$PG_CLIENT_LIB${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"; fi
OUT=${OUT:?OUT wajib diisi (folder bukti)}
BASE=${DATABASE_URL%/postgres}
SRC=itsm_bk_src; DST=itsm_bk_dst
API=./target/debug/itsm-api
P() { "$B/psql" -X -q -v ON_ERROR_STOP=1 "$@"; }
echo "sha: $(git rev-parse HEAD) dirty: $(git status --porcelain | wc -l)"
echo "pg_dump: $("$B/pg_dump" --version)"; echo "pg_restore: $("$B/pg_restore" --version)"
echo "server: $(P "$DATABASE_URL" -Atc 'show server_version')"
P "$DATABASE_URL" -c "CREATE DATABASE $SRC" -c "CREATE DATABASE $DST"
export ITSM_ENV=uat ITSM_ACCESS_MODE=uat_superadmin
DATABASE_URL="$BASE/$SRC" "$API" migrate
DATABASE_URL="$BASE/$SRC" ITSM_SEED_CONFIRM_DB=$SRC "$API" seed-uat
# 0011: platform refusals must survive the round trip as data, not only as schema.
P "$BASE/$SRC" -c "insert into platform_audit_events (actor_id, action, capability, method, route, outcome, correlation_id)
  select id, 'platform.access.denied', 'platform.companies', m, r, 'denied', 'bk-probe-'||m
  from (select id from users order by id limit 1) u,
       (values ('GET','/api/v1/companies'), ('PATCH','/api/v1/companies/{company_id}')) v(m, r)"
echo "src platform_audit_events rows: $(P "$BASE/$SRC" -Atc 'select count(*) from platform_audit_events')"
"$B/pg_dump" -Fc --no-owner -d "$BASE/$SRC" -f "$OUT/itsm_bk_src.dump"
echo "dump bytes: $(stat -c %s "$OUT/itsm_bk_src.dump") sha256: $(sha256sum "$OUT/itsm_bk_src.dump" | cut -d' ' -f1)"
"$B/pg_restore" --list "$OUT/itsm_bk_src.dump" > "$OUT/restore-list.txt"
echo "toc entries: $(grep -vc '^;' "$OUT/restore-list.txt")"
echo "toc platform_audit_events: $(grep -c 'platform_audit_events' "$OUT/restore-list.txt")"
"$B/pg_restore" --no-owner --exit-on-error -d "$BASE/$DST" "$OUT/itsm_bk_src.dump"
snap() { # $1 db  -> canonical snapshot
  local u="$BASE/$1"
  echo "## tables+rows"
  for t in $(P "$u" -Atc "select quote_ident(schemaname)||'.'||quote_ident(tablename) from pg_tables where schemaname='public' order by 1"); do
    echo "$t $(P "$u" -Atc "select count(*) from $t") $(P "$u" -Atc "select md5(coalesce(string_agg(r::text, '|' order by r::text),'')) from $t r")"
  done
  echo "## constraints"
  P "$u" -Atc "select conrelid::regclass::text||' '||conname::text||' '||contype::text||' '||pg_get_constraintdef(oid) from pg_constraint where connamespace='public'::regnamespace order by 1"
  echo "## indexes"
  P "$u" -Atc "select indexdef from pg_indexes where schemaname='public' order by 1"
  echo "## triggers"
  P "$u" -Atc "select tgrelid::regclass::text||' '||tgname||' enabled='||tgenabled::text||' '||pg_get_triggerdef(oid) from pg_trigger where not tgisinternal order by 1"
  echo "## functions"
  P "$u" -Atc "select p.oid::regprocedure::text||' '||md5(pg_get_functiondef(p.oid)) from pg_proc p where pronamespace='public'::regnamespace order by 1"
  echo "## migrations"
  P "$u" -Atc "select version::text||' '||description||' '||success::text||' '||encode(checksum,'hex') from _sqlx_migrations order by version"
}
snap $SRC > "$OUT/snapshot-src.txt"; snap $DST > "$OUT/snapshot-dst.txt"
for s in "tables+rows" constraints indexes triggers functions migrations; do
  n=$(awk -v s="## $s" '$0==s{f=1;next} /^## /{f=0} f' "$OUT/snapshot-src.txt" | wc -l); echo "$s: $n"
done
RAW_CHANGED=$(diff "$OUT/snapshot-src.txt" "$OUT/snapshot-dst.txt" | grep -c "^<" || true)
diff -u "$OUT/snapshot-src.txt" "$OUT/snapshot-dst.txt" > "$OUT/snapshot.diff" || true
echo "raw differing lines: $RAW_CHANGED ($(grep "^-[a-z]" "$OUT/snapshot.diff" | cut -d" " -f1-2 | tr "\n" ";"))"
norm() { tr -d "()" < "$1"; }
if diff <(norm "$OUT/snapshot-src.txt") <(norm "$OUT/snapshot-dst.txt") >/dev/null; then echo "SNAPSHOT IDENTICAL after removing redundant parentheses (deparse only)"; else echo "SNAPSHOT DIFFERS"; exit 1; fi
grep "^public.platform_audit_events \|^platform_audit_events " "$OUT/snapshot-dst.txt" | sed 's/^/dst: /'
# constraint still enforced after restore (dup unique must fail)
if P "$BASE/$DST" -c "insert into workspaces select * from workspaces limit 1" 2>"$OUT/dup.err"; then echo "PK NOT ENFORCED"; exit 1; else echo "unique enforced after restore: $(head -1 "$OUT/dup.err" | cut -c1-80)"; fi
# ---- 0011 platform_audit_events after restore -------------------------------------------
for db in $SRC $DST; do
  en=$(P "$BASE/$db" -Atc "select string_agg(tgname||'='||tgenabled::text, ',' order by tgname) from pg_trigger where tgrelid='platform_audit_events'::regclass and not tgisinternal")
  [ "$en" = "platform_audit_events_append_only=O,platform_audit_events_no_truncate=O" ] || { echo "TRIGGERS NOT ENABLED $db: $en"; exit 1; }
  echo "$db platform triggers: $en"
  before=$(P "$BASE/$db" -Atc "select count(*)||' '||md5(string_agg(r::text,'|' order by r::text)) from platform_audit_events r")
  for stmt in "update platform_audit_events set correlation_id = 'tampered'" "delete from platform_audit_events" "truncate platform_audit_events"; do
    P "$BASE/$db" -c "do \$\$ begin $stmt; raise exception 'NOT BLOCKED'; exception when sqlstate 'IT422' then null; end \$\$"
    echo "$db refuses (IT422): $stmt"
  done
  after=$(P "$BASE/$db" -Atc "select count(*)||' '||md5(string_agg(r::text,'|' order by r::text)) from platform_audit_events r")
  [ "$before" = "$after" ] || { echo "ROWS CHANGED $db"; exit 1; }
  echo "$db platform rows unchanged: $after"
  P "$BASE/$db" -c "do \$\$ begin insert into platform_audit_events (actor_id, action, capability, method, route, outcome)
    select id, 'platform.access.granted', 'platform.companies', 'GET', '/api/v1/companies', 'denied' from users limit 1;
    raise exception 'CHECK NOT ENFORCED'; exception when check_violation then null; end \$\$"
  P "$BASE/$db" -c "do \$\$ begin insert into platform_audit_events (actor_id, action, capability, method, route, outcome)
    values (gen_random_uuid(), 'platform.access.denied', 'platform.companies', 'GET', '/api/v1/companies', 'denied');
    raise exception 'FK NOT ENFORCED'; exception when foreign_key_violation then null; end \$\$"
  echo "$db platform CHECK (action) and FK (actor) enforced"
  P "$BASE/$db" -c "insert into platform_audit_events (actor_id, action, capability, method, route, outcome, correlation_id)
    select id, 'platform.access.denied', 'platform.companies', 'POST', '/api/v1/companies', 'denied', 'bk-after-restore' from users order by id limit 1"
  echo "$db valid platform insert accepted (append still works)"
done
# ---- every append-only table (itsm_append_only) refuses TRUNCATE on the restored DB, and
# ---- DELETE wherever it holds rows (row triggers do not fire on empty tables) --------------
for t in $(P "$BASE/$DST" -Atc "select distinct tgrelid::regclass::text from pg_trigger t join pg_proc p on p.oid=t.tgfoid where p.proname='itsm_append_only' and not tgisinternal order by 1"); do
  rows=$(P "$BASE/$DST" -Atc "select count(*) from $t")
  trunc=$(P "$BASE/$DST" -Atc "select count(*) from pg_trigger t join pg_proc p on p.oid=t.tgfoid where p.proname='itsm_append_only' and tgrelid='$t'::regclass and (tgtype & 32) <> 0")
  res="$t rows=$rows"
  if [ "$rows" -gt 0 ]; then
    P "$BASE/$DST" -c "do \$\$ begin delete from $t; raise exception 'NOT BLOCKED'; exception when sqlstate 'IT422' then null; end \$\$"; res="$res delete=IT422"
  else res="$res delete=skip(empty)"; fi
  if [ "$trunc" -gt 0 ]; then
    P "$BASE/$DST" -c "do \$\$ begin truncate $t cascade; raise exception 'NOT BLOCKED'; exception when sqlstate 'IT422' then null; end \$\$"; res="$res truncate=IT422"
  else res="$res truncate=no-trigger"; fi
  echo "dst append-only $res"
done
DATABASE_URL="$BASE/$DST" "$API" migrate && echo "migrate on restored: ok (no-op)"
P "$BASE/$DST" -Atc "select 'migrations after: '||count(*)||' max '||max(version) from _sqlx_migrations"
P "$BASE/$DST" -Atc "select 'seed tickets: '||count(*) from tickets"
P "$BASE/$DST" -Atc "select 'dst platform_audit_events rows: '||count(*) from platform_audit_events"
echo "BACKUP-RESTORE PASS"
