//! CP7c performance tooling. Not part of the image (the Dockerfile copies `itsm-api` only).
//!
//! `itsm-perf seed [--tickets N] [--notes-per-ticket N] [--users N]`
//!   Fills a marked dummy database (`ITSM_SEED_CONFIRM_DB`, `itsm_deployment_marker`) with
//!   volume data: a third dummy company, `--users` members spread over DUMA/DUMB/DUMC and
//!   `--tickets` tickets (50/30/20 %) received over two years, with resolve events and notes.
//!   Rows are generated in SQL batches with user triggers bypassed
//!   (`session_replication_role = replica`, superuser on a throwaway cluster), so the seed
//!   verifies the invariants the triggers and foreign keys would have enforced afterwards and
//!   fails when any is broken.
//!
//! `itsm-perf load --base URL [--users N] [--duration-secs N] [--warmup-secs N]
//!   [--think-ms N] [--out FILE]`
//!   Drives the HTTP API with N virtual users (one session each, issued in the database),
//!   a weighted mix of list/search/board/dashboard/detail/notes reads and note/transition/
//!   create writes, and prints p50/p95/p99 per scenario against the CP7c targets.
//!
//! `itsm-perf verify` re-runs the seed invariants (read only), e.g. after a load test.
//!
//! `itsm-perf stats [--reset]` prints the slowest statements from `pg_stat_statements`.
//!
//! All data is labelled dummy; `DATABASE_URL` comes from the environment and is never printed.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use tokio::sync::Mutex;
use uuid::Uuid;

type Res<T> = Result<T, String>;

const PERF_WS: [(&str, &str, u64); 3] = [
    ("DUMA", "PT Dummy Alfa (uji)", 50),
    ("DUMB", "PT Dummy Beta (uji)", 30),
    ("DUMC", "PT Dummy Charlie (uji perf)", 20),
];
const BATCH: i64 = 50_000;
/// Every ticket whose number is a multiple of this gets `HOT_NOTES` notes (long timelines).
const HOT_EVERY: i64 = 10_000;
const HOT_NOTES: i64 = 300;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("runtime: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match rt.block_on(run(args)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("itsm-perf: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

struct Opts(BTreeMap<String, String>);

impl Opts {
    fn parse(args: &[String]) -> Res<Opts> {
        let mut m = BTreeMap::new();
        let mut it = args.iter();
        while let Some(k) = it.next() {
            let key = k
                .strip_prefix("--")
                .ok_or(format!("argumen tidak dikenal: {k}"))?;
            if key == "reset" {
                m.insert(key.to_owned(), "1".to_owned());
                continue;
            }
            let v = it.next().ok_or(format!("--{key} butuh nilai"))?;
            m.insert(key.to_owned(), v.clone());
        }
        Ok(Opts(m))
    }
    fn num(&self, key: &str, default: i64, min: i64, max: i64) -> Res<i64> {
        match self.0.get(key) {
            None => Ok(default),
            Some(v) => v
                .parse::<i64>()
                .ok()
                .filter(|n| (min..=max).contains(n))
                .ok_or(format!("--{key} harus {min}..={max}")),
        }
    }
    fn text(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }
    fn check(&self, allowed: &[&str]) -> Res<()> {
        match self.0.keys().find(|k| !allowed.contains(&k.as_str())) {
            Some(k) => Err(format!("opsi tidak dikenal: --{k}")),
            None => Ok(()),
        }
    }
}

async fn pool(max: u32) -> Res<PgPool> {
    let url = std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL wajib diisi".to_owned())?;
    PgPoolOptions::new()
        .max_connections(max)
        .connect(&url)
        .await
        .map_err(|e| format!("database connect failed: {e}"))
}

async fn run(args: Vec<String>) -> Res<()> {
    let (cmd, rest) = args
        .split_first()
        .ok_or("perintah: seed | load | verify | stats")?;
    let o = Opts::parse(rest)?;
    match cmd.as_str() {
        "seed" => {
            o.check(&["tickets", "notes-per-ticket", "users"])?;
            seed(&o).await
        }
        "load" => {
            o.check(&[
                "base",
                "users",
                "duration-secs",
                "warmup-secs",
                "think-ms",
                "out",
            ])?;
            load(&o).await
        }
        "verify" => {
            o.check(&[])?;
            verify(&pool(2).await?).await
        }
        "stats" => {
            o.check(&["reset", "top"])?;
            stats(&o).await
        }
        other => Err(format!("perintah tidak dikenal: {other}")),
    }
}

// ---------------------------------------------------------------- seed

const SUBJECTS: &str = "ARRAY['Transfer gagal ke rekening tujuan', 'Saldo terpotong tetapi transaksi gagal', \
  'Tidak bisa login aplikasi', 'QRIS merchant tidak terbaca', 'Pengembalian dana belum masuk', \
  'Notifikasi transaksi tidak muncul', 'Kode OTP tidak diterima', 'Limit harian tidak sesuai', \
  'Mutasi rekening tidak tampil', 'Settlement merchant terlambat', 'Kartu terblokir setelah salah PIN', \
  'Top up e-wallet tertunda', 'Biaya admin tertagih dua kali', 'Virtual account kedaluwarsa', \
  'Perubahan nomor ponsel', 'Aplikasi keluar sendiri saat bayar', 'Tagihan PLN tidak terbayar', \
  'Penarikan tunai ATM gagal', 'Data nasabah tidak sinkron', 'Permintaan dokumen rekening koran']";
const MERCHANTS: &str = "ARRAY['Toko Uji Satu', 'Warung Dummy', 'Apotek Contoh', 'Kafe Simulasi', \
  'Bengkel Uji', 'Minimarket Dummy', 'Laundry Contoh', 'Katering Uji', 'Butik Simulasi', \
  'Percetakan Dummy', 'Klinik Contoh', 'Agen Uji']";

async fn seed(o: &Opts) -> Res<()> {
    let tickets = o.num("tickets", 1_000_000, 3, 5_000_000)?;
    let notes = o.num("notes-per-ticket", 9, 0, 30)?;
    let users = o.num("users", 1000, 3, 10_000)?;
    let confirm = std::env::var("ITSM_SEED_CONFIRM_DB")
        .map_err(|_| "ITSM_SEED_CONFIRM_DB wajib diisi".to_owned())?;
    let db = pool(4).await?;
    let e = |e: sqlx::Error| e.to_string();

    // Same marking as `seed-uat`, then the stricter guards of this tool.
    itsm_api::seed::seed_uat(&db, &confirm)
        .await
        .map_err(|e| e.to_string())?;
    let (current, marked, superuser): (String, Option<String>, String) = sqlx::query_as(
        "SELECT current_database(), \
           (SELECT marked_db FROM itsm_deployment_marker WHERE kind = 'test_dummy'), \
           current_setting('is_superuser')",
    )
    .fetch_one(&db)
    .await
    .map_err(e)?;
    if current != confirm || marked.as_deref() != Some(current.as_str()) {
        return Err("database tujuan bukan database dummy yang dikonfirmasi".into());
    }
    if superuser != "on" {
        return Err("seed perf butuh superuser (klaster Postgres sementara)".into());
    }

    // Third dummy company (the target has three companies).
    let (code, name, _) = PERF_WS[2];
    let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM workspaces WHERE code = $1")
        .bind(code)
        .fetch_optional(&db)
        .await
        .map_err(e)?;
    if exists.is_none() {
        let mut tx = db.begin().await.map_err(e)?;
        let company: Uuid =
            sqlx::query_scalar("INSERT INTO companies (code, name) VALUES ($1, $2) RETURNING id")
                .bind(code)
                .bind(name)
                .fetch_one(&mut *tx)
                .await
                .map_err(e)?;
        sqlx::query("INSERT INTO workspaces (company_id, code, name) VALUES ($1, $2, $3)")
            .bind(company)
            .bind(code)
            .bind(name)
            .execute(&mut *tx)
            .await
            .map_err(e)?;
        tx.commit().await.map_err(e)?;
    }
    let mut ws = Vec::new();
    for (code, _, share) in PERF_WS {
        let id: Uuid = sqlx::query_scalar("SELECT id FROM workspaces WHERE code = $1")
            .bind(code)
            .fetch_one(&db)
            .await
            .map_err(e)?;
        ws.push((code, id, share));
    }
    let existing: i64 =
        sqlx::query_scalar("SELECT count(*) FROM tickets WHERE workspace_id = ANY($1)")
            .bind(ws.iter().map(|w| w.1).collect::<Vec<_>>())
            .fetch_one(&db)
            .await
            .map_err(e)?;
    if existing > 0 {
        return Err(format!(
            "workspace perf sudah berisi {existing} tiket; seed hanya sekali"
        ));
    }

    // Catalogs of DUMC copied from DUMA, plus ten extra categories everywhere.
    for table in [
        "priorities",
        "intake_channels",
        "categories",
        "products",
        "receiving_units",
        "regions",
    ] {
        // Table names come from the list above, never from input.
        sqlx::query(&format!(
            "INSERT INTO {table} (workspace_id, code, label, sort_order) \
             SELECT $2, code, label, sort_order FROM {table} WHERE workspace_id = $1 \
             ON CONFLICT (workspace_id, code) DO NOTHING"
        ))
        .bind(ws[0].1)
        .bind(ws[2].1)
        .execute(&db)
        .await
        .map_err(e)?;
    }
    sqlx::query(
        "INSERT INTO categories (workspace_id, code, label, sort_order) \
         SELECT w, 'perf_kat_' || lpad(i::text, 2, '0'), 'Kategori perf ' || i || ' (dummy)', 100 + i \
         FROM unnest($1::uuid[]) AS w, generate_series(1, 10) AS i \
         ON CONFLICT (workspace_id, code) DO NOTHING",
    )
    .bind(ws.iter().map(|w| w.1).collect::<Vec<_>>())
    .execute(&db)
    .await
    .map_err(e)?;

    // Users: i % 10 → 0-4 DUMA, 5-7 DUMB, 8-9 DUMC; every 20th is lead_cs.
    sqlx::query(
        "INSERT INTO users (external_subject, display_name) \
         SELECT 'perf:user-' || lpad(i::text, 5, '0'), 'Pengguna perf ' || i || ' (dummy)' \
         FROM generate_series(1, $1) AS i ON CONFLICT (external_subject) DO NOTHING",
    )
    .bind(users)
    .execute(&db)
    .await
    .map_err(e)?;
    sqlx::query(
        "INSERT INTO workspace_memberships (workspace_id, user_id, role) \
         SELECT CASE WHEN n % 10 < 5 THEN $1 WHEN n % 10 < 8 THEN $2 ELSE $3 END, u.id, \
                CASE WHEN n % 20 = 0 THEN 'lead_cs' ELSE 'cs' END \
         FROM (SELECT id, substr(external_subject, 11)::int AS n FROM users \
               WHERE external_subject LIKE 'perf:user-%') u \
         ON CONFLICT (workspace_id, user_id) DO NOTHING",
    )
    .bind(ws[0].1)
    .bind(ws[1].1)
    .bind(ws[2].1)
    .execute(&db)
    .await
    .map_err(e)?;
    let creator: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE external_subject = 'dummy:uat-superadmin'")
            .fetch_one(&db)
            .await
            .map_err(e)?;

    let started = Instant::now();
    for (i, (code, id, share)) in ws.iter().enumerate() {
        let n = tickets * i64::try_from(*share).unwrap_or(0) / 100;
        let mut done = 0;
        while done < n {
            let size = BATCH.min(n - done);
            let mut tx = db.begin().await.map_err(e)?;
            sqlx::query("SET LOCAL session_replication_role = replica")
                .execute(&mut *tx)
                .await
                .map_err(e)?;
            // Reproducible: the seed depends on workspace and batch only.
            #[allow(clippy::cast_precision_loss)]
            let s = ((i as i64 * 7919 + done / BATCH) % 1000) as f64 / 1000.0;
            sqlx::query("SELECT setseed($1)")
                .bind(s)
                .execute(&mut *tx)
                .await
                .map_err(e)?;
            sqlx::query(&tickets_sql())
                .bind(id)
                .bind(code)
                .bind(done)
                .bind(size)
                .bind(n)
                .bind(creator)
                .execute(&mut *tx)
                .await
                .map_err(e)?;
            sqlx::query(
                "INSERT INTO ticket_resolution_events (workspace_id, ticket_id, cycle_no, action, \
                   summary, actor_id, occurred_at) \
                 SELECT workspace_id, id, 1, 'resolve', 'Ringkasan penyelesaian uji perf (dummy).', \
                   COALESCE(owner_user_id, created_by), updated_at \
                 FROM tickets WHERE workspace_id = $1 AND number > $2 AND number <= $3 \
                   AND workflow_status = 'resolved'",
            )
            .bind(id)
            .bind(done)
            .bind(done + size)
            .execute(&mut *tx)
            .await
            .map_err(e)?;
            if notes > 0 {
                sqlx::query(
                    "INSERT INTO ticket_activities (workspace_id, ticket_id, actor_id, activity_type, \
                       body, idempotency_key, request_hash, created_at) \
                     SELECT t.workspace_id, t.id, COALESCE(t.owner_user_id, t.created_by), 'note', \
                       'Catatan tindak lanjut uji perf (dummy) ke-' || i || '.', \
                       'perf-' || t.number || '-' || i, \
                       sha256(convert_to('perf-' || t.number || '-' || i, 'UTF8')), \
                       t.received_at + (t.updated_at - t.received_at) * (i::float8 / (t.n + 1)) \
                     FROM (SELECT t.*, CASE WHEN t.number % $5 = 0 THEN $6 \
                             ELSE 1 + floor(random() * (2 * $4 - 1))::int END AS n \
                           FROM tickets t WHERE workspace_id = $1 AND number > $2 AND number <= $3) t, \
                       generate_series(1, t.n) AS i",
                )
                .bind(id)
                .bind(done)
                .bind(done + size)
                .bind(notes)
                .bind(HOT_EVERY)
                .bind(HOT_NOTES)
                .execute(&mut *tx)
                .await
                .map_err(e)?;
            }
            tx.commit().await.map_err(e)?;
            done += size;
            println!(
                "seed {code}: {done}/{n} tiket ({:.0} dtk)",
                started.elapsed().as_secs_f64()
            );
        }
        sqlx::query(
            "UPDATE workspace_ticket_counters SET last_number = $2 WHERE workspace_id = $1",
        )
        .bind(id)
        .bind(n)
        .execute(&db)
        .await
        .map_err(e)?;
    }
    for table in [
        "tickets",
        "ticket_resolution_events",
        "ticket_activities",
        "users",
        "workspace_memberships",
    ] {
        sqlx::query(&format!("VACUUM (ANALYZE) {table}"))
            .execute(&db)
            .await
            .map_err(e)?;
    }
    verify(&db).await?;
    let sizes: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT c.relname::text, c.reltuples::bigint, pg_size_pretty(pg_total_relation_size(c.oid)) \
         FROM pg_class c WHERE c.relname IN ('tickets', 'ticket_resolution_events', 'ticket_activities', \
           'users', 'workspace_memberships') ORDER BY 1",
    )
    .fetch_all(&db)
    .await
    .map_err(e)?;
    for (t, rows, size) in sizes {
        println!("tabel {t}: ~{rows} baris, {size}");
    }
    println!(
        "seed selesai dalam {:.0} dtk",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// `$1` workspace, `$2` code, `$3` offset, `$4` batch size, `$5` workspace total, `$6` creator.
/// Ticket k of N is received `730 days × (1 − k/N)²` ago (denser towards now); the status mix
/// depends on age (most old tickets resolved). Owners, channels, priorities and categories are
/// picked from the workspace's own rows.
fn tickets_sql() -> String {
    format!(
        "WITH m AS (SELECT array_agg(user_id ORDER BY user_id) AS a FROM workspace_memberships \
                    WHERE workspace_id = $1 AND active), \
              ch AS (SELECT array_agg(id ORDER BY sort_order, id) AS a FROM intake_channels WHERE workspace_id = $1), \
              pr AS (SELECT array_agg(id ORDER BY sort_order, id) AS a FROM priorities WHERE workspace_id = $1), \
              ca AS (SELECT array_agg(id ORDER BY sort_order, id) AS a FROM categories WHERE workspace_id = $1), \
              g AS (SELECT k, random() AS r1, random() AS r2, random() AS r3, random() AS r4, \
                      random() AS r5, random() AS r6, random() AS r7, random() AS r8, \
                      now() - interval '730 days' * power(1 - k::float8 / $5, 2) \
                        - interval '1 minute' * (1 + floor(random() * 60)) AS recv \
                    FROM generate_series($3::bigint + 1, $3::bigint + $4) AS k), \
              s AS (SELECT g.*, CASE \
                      WHEN now() - recv < interval '7 days' THEN CASE WHEN r1 < 0.4 THEN 'resolved' \
                        WHEN r1 < 0.55 THEN 'new' WHEN r1 < 0.8 THEN 'open' \
                        WHEN r1 < 0.92 THEN 'pending_customer' ELSE 'pending_engineering' END \
                      WHEN now() - recv < interval '30 days' THEN CASE WHEN r1 < 0.85 THEN 'resolved' \
                        WHEN r1 < 0.9 THEN 'open' WHEN r1 < 0.96 THEN 'pending_customer' \
                        ELSE 'pending_engineering' END \
                      ELSE CASE WHEN r1 < 0.99 THEN 'resolved' ELSE 'pending_engineering' END END AS st \
                    FROM g) \
         INSERT INTO tickets (workspace_id, number, number_prefix, subject, chronology, received_at, \
           intake_channel_id, priority_id, category_id, owner_user_id, workflow_status, cycle_no, \
           version, created_by, created_at, updated_at) \
         SELECT $1, k, $2, \
           ({SUBJECTS})[1 + floor(r2 * 20)::int] || ' - ' || ({MERCHANTS})[1 + floor(r3 * 12)::int] \
             || ' #' || substr(md5(k::text), 1, 8), \
           'Kronologi uji perf (dummy) tiket ' || k || '.', \
           recv, \
           ch.a[1 + floor(r4 * cardinality(ch.a))::int], \
           CASE WHEN r5 < 0.05 THEN NULL ELSE pr.a[1 + floor(r5 * cardinality(pr.a))::int] END, \
           CASE WHEN r6 < 0.1 THEN NULL ELSE ca.a[1 + floor(r6 * cardinality(ca.a))::int] END, \
           CASE WHEN st = 'new' AND r7 < 0.6 THEN NULL WHEN r7 < 0.05 THEN NULL \
             ELSE m.a[1 + floor(r7 * cardinality(m.a))::int] END, \
           st, 1, CASE WHEN st = 'new' THEN 1 ELSE 2 END, $6, \
           LEAST(now(), recv + interval '2 minutes'), \
           LEAST(now(), recv + interval '2 minutes' + interval '1 hour' * floor(r8 * 72)) \
         FROM s, m, ch, pr, ca"
    )
}

/// What the bypassed triggers and foreign keys guarantee; every count must be 0.
async fn verify(db: &PgPool) -> Res<()> {
    let checks: &[(&str, &str)] = &[
        (
            "owner bukan anggota aktif workspace",
            "SELECT count(*) FROM tickets t WHERE t.owner_user_id IS NOT NULL AND NOT EXISTS ( \
               SELECT 1 FROM workspace_memberships m WHERE m.workspace_id = t.workspace_id \
               AND m.user_id = t.owner_user_id AND m.active)",
        ),
        (
            "katalog dari workspace lain",
            "SELECT count(*) FROM tickets t WHERE \
               (t.intake_channel_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM intake_channels c \
                  WHERE c.workspace_id = t.workspace_id AND c.id = t.intake_channel_id)) \
               OR (t.priority_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM priorities c \
                  WHERE c.workspace_id = t.workspace_id AND c.id = t.priority_id)) \
               OR (t.category_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM categories c \
                  WHERE c.workspace_id = t.workspace_id AND c.id = t.category_id))",
        ),
        (
            "resolved tanpa event resolve",
            "SELECT count(*) FROM tickets t WHERE t.workflow_status = 'resolved' AND NOT EXISTS ( \
               SELECT 1 FROM ticket_resolution_events e WHERE e.ticket_id = t.id \
               AND e.cycle_no = t.cycle_no AND e.action = 'resolve')",
        ),
        (
            "event resolve pada tiket yang tidak resolved",
            "SELECT count(*) FROM ticket_resolution_events e JOIN tickets t \
               ON t.workspace_id = e.workspace_id AND t.id = e.ticket_id \
               WHERE e.action = 'resolve' AND t.workflow_status <> 'resolved'",
        ),
        (
            "catatan tanpa tiket di workspace yang sama",
            "SELECT count(*) FROM ticket_activities a WHERE NOT EXISTS (SELECT 1 FROM tickets t \
               WHERE t.workspace_id = a.workspace_id AND t.id = a.ticket_id)",
        ),
        (
            "counter nomor di bawah nomor tertinggi",
            "SELECT count(*) FROM workspace_ticket_counters c WHERE c.last_number < \
               COALESCE((SELECT max(number) FROM tickets t WHERE t.workspace_id = c.workspace_id), 0)",
        ),
        (
            "prefix nomor berbeda dari kode workspace",
            "SELECT count(*) FROM tickets t JOIN workspaces w ON w.id = t.workspace_id \
               WHERE t.number_prefix <> w.code",
        ),
        (
            "hitungan status (0030) berbeda dari tiket",
            "SELECT count(*) FROM ( \
               SELECT workspace_id, workflow_status, count(*) AS n FROM tickets GROUP BY 1, 2) t \
             FULL JOIN ( \
               SELECT workspace_id, workflow_status, sum(n) AS n FROM ticket_status_counts \
               GROUP BY 1, 2 HAVING sum(n) <> 0) c USING (workspace_id, workflow_status) \
             WHERE t.n IS DISTINCT FROM c.n",
        ),
    ];
    let mut bad = 0;
    for (label, sql) in checks {
        let n: i64 = sqlx::query_scalar(sql)
            .fetch_one(db)
            .await
            .map_err(|e| e.to_string())?;
        println!("cek integritas: {label}: {n}");
        if n != 0 {
            bad += 1;
        }
    }
    if bad > 0 {
        return Err(format!("{bad} cek integritas gagal"));
    }
    Ok(())
}

// ---------------------------------------------------------------- load

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Class {
    Read,
    List,
    Write,
}

impl Class {
    fn target_ms(self) -> f64 {
        match self {
            Class::Read => 300.0,
            Class::Write => 600.0,
            Class::List => 1500.0,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Class::Read => "baca",
            Class::Write => "tulis",
            Class::List => "daftar",
        }
    }
}

/// (scenario, class, weight).
const MIX: &[(&str, Class, u32)] = &[
    ("list_default", Class::List, 20),
    ("list_next_page", Class::List, 8),
    ("list_active_mine", Class::List, 8),
    ("list_filtered_range", Class::List, 5),
    ("search_text", Class::List, 6),
    ("search_number", Class::List, 3),
    ("board", Class::List, 5),
    ("dashboard", Class::List, 7),
    ("detail", Class::Read, 12),
    ("detail_hot", Class::Read, 2),
    ("notes", Class::Read, 6),
    ("activity", Class::Read, 4),
    ("note_create", Class::Write, 6),
    ("transition", Class::Write, 3),
    ("ticket_create", Class::Write, 3),
];

const SEARCH_WORDS: &[&str] = &[
    "QRIS",
    "transfer",
    "OTP",
    "settlement",
    "Warung",
    "login",
    "refund",
    "PLN",
    "saldo",
    "dana",
];

struct Ws {
    id: Uuid,
    sample: Vec<(Uuid, i64)>,
    active: Vec<Uuid>,
    hot: Vec<Uuid>,
    channel: Uuid,
    priority: Uuid,
}

struct User {
    id: Uuid,
    ws: usize,
    cookie: String,
    csrf: String,
}

#[derive(Default)]
struct Stat {
    ms: Vec<f64>,
    statuses: BTreeMap<u16, u64>,
    errors: u64,
    /// Failure reason (HTTP code or transport error kind) -> count.
    failures: BTreeMap<String, u64>,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % (n.max(1) as u64)).unwrap_or(0)
    }
}

async fn load(o: &Opts) -> Res<()> {
    let base = o
        .text("base")
        .ok_or("--base wajib (mis. http://127.0.0.1:8080)")?
        .to_owned();
    let n_users = o.num("users", 300, 1, 5000)?;
    let duration =
        Duration::from_secs(u64::try_from(o.num("duration-secs", 180, 5, 7200)?).unwrap_or(180));
    let warmup =
        Duration::from_secs(u64::try_from(o.num("warmup-secs", 20, 0, 600)?).unwrap_or(20));
    let think = u64::try_from(o.num("think-ms", 1000, 0, 60_000)?).unwrap_or(1000);
    let db = pool(8).await?;
    let e = |e: sqlx::Error| e.to_string();

    let mut wss = Vec::new();
    for (code, _, _) in PERF_WS {
        let id: Uuid = sqlx::query_scalar("SELECT id FROM workspaces WHERE code = $1")
            .bind(code)
            .fetch_one(&db)
            .await
            .map_err(e)?;
        let sample: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT id, number FROM tickets \
             WHERE workspace_id = $1 ORDER BY md5(id::text) LIMIT 3000",
        )
        .bind(id)
        .fetch_all(&db)
        .await
        .map_err(e)?;
        let active: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM tickets WHERE workspace_id = $1 \
               AND workflow_status IN ('open', 'pending_customer') ORDER BY md5(id::text) LIMIT 3000",
        )
        .bind(id)
        .fetch_all(&db)
        .await
        .map_err(e)?;
        let hot: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM tickets WHERE workspace_id = $1 AND number % $2 = 0 ORDER BY number",
        )
        .bind(id)
        .bind(HOT_EVERY)
        .fetch_all(&db)
        .await
        .map_err(e)?;
        let channel: Uuid = sqlx::query_scalar(
            "SELECT id FROM intake_channels WHERE workspace_id = $1 AND code = 'email'",
        )
        .bind(id)
        .fetch_one(&db)
        .await
        .map_err(e)?;
        let priority: Uuid = sqlx::query_scalar(
            "SELECT id FROM priorities WHERE workspace_id = $1 AND code = 'normal'",
        )
        .bind(id)
        .fetch_one(&db)
        .await
        .map_err(e)?;
        if sample.is_empty() || active.is_empty() {
            return Err(format!(
                "workspace {code} belum di-seed (jalankan `itsm-perf seed`)"
            ));
        }
        wss.push(Ws {
            id,
            sample,
            active,
            hot,
            channel,
            priority,
        });
    }
    let members: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT u.id, m.workspace_id FROM users u JOIN workspace_memberships m ON m.user_id = u.id \
         WHERE u.external_subject LIKE 'perf:user-%' AND m.active AND m.role = 'cs' \
         ORDER BY u.external_subject LIMIT $1",
    )
    .bind(n_users)
    .fetch_all(&db)
    .await
    .map_err(e)?;
    if i64::try_from(members.len()).unwrap_or(0) < n_users {
        return Err("pengguna perf kurang (jalankan `itsm-perf seed --users`)".into());
    }
    let mut users = Vec::new();
    for (uid, ws_id) in members {
        let s = itsm_api::session::issue(&db, uid, "uat_superadmin")
            .await
            .map_err(|e| format!("issue session: {e:?}"))?;
        let ws = wss
            .iter()
            .position(|w| w.id == ws_id)
            .ok_or("workspace anggota tak dikenal")?;
        users.push(User {
            id: uid,
            ws,
            cookie: format!("{}={}", itsm_api::session::cookie_name(false), s.token),
            csrf: s.csrf_token,
        });
    }
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(usize::try_from(n_users).unwrap_or(300))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let wss = Arc::new(wss);
    let stats: Arc<Mutex<BTreeMap<&'static str, Stat>>> = Arc::default();
    let t0 = Instant::now();
    let measure_from = t0 + warmup;
    let deadline = measure_from + duration;
    println!(
        "load: {n_users} pengguna, pemanasan {} dtk, ukur {} dtk, jeda {think} ms",
        warmup.as_secs(),
        duration.as_secs()
    );
    let mut tasks = Vec::new();
    for (i, u) in users.into_iter().enumerate() {
        let (client, wss, stats, base) = (client.clone(), wss.clone(), stats.clone(), base.clone());
        tasks.push(tokio::spawn(async move {
            let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ ((i as u64 + 1) * 0x1000_0001));
            // Spread the start over the first second so users do not fire in lockstep.
            tokio::time::sleep(Duration::from_millis(rng.next() % 1000)).await;
            let mut cursor: Option<String> = None;
            let total: u32 = MIX.iter().map(|m| m.2).sum();
            while Instant::now() < deadline {
                let mut pick = u32::try_from(rng.next() % u64::from(total)).unwrap_or(0);
                let (name, _, _) = *MIX
                    .iter()
                    .find(|m| {
                        if pick < m.2 {
                            true
                        } else {
                            pick -= m.2;
                            false
                        }
                    })
                    .unwrap_or(&MIX[0]);
                let ws = &wss[u.ws];
                let started = Instant::now();
                let r = scenario(name, &client, &base, &u, ws, &mut rng, &mut cursor).await;
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                if started >= measure_from && Instant::now() <= deadline + Duration::from_secs(30) {
                    let mut s = stats.lock().await;
                    let st = s.entry(name).or_default();
                    st.ms.push(ms);
                    match r {
                        Ok(code) => *st.statuses.entry(code).or_default() += 1,
                        Err(e) => {
                            st.errors += 1;
                            let reason =
                                e.strip_prefix(name).unwrap_or(&e).trim_start_matches(": ");
                            let reason: String = reason.chars().take(60).collect();
                            *st.failures.entry(reason).or_default() += 1;
                        }
                    }
                }
                if think > 0 {
                    let jitter = think / 2 + rng.next() % (think.max(1));
                    tokio::time::sleep(Duration::from_millis(jitter)).await;
                }
            }
        }));
    }
    for t in tasks {
        let _ = t.await;
    }
    let measured = duration.as_secs_f64();
    report(
        &stats.lock().await.iter().collect::<Vec<_>>(),
        measured,
        n_users,
        think,
        o.text("out"),
    )
}

fn ok_code(name: &str, code: u16) -> bool {
    match name {
        "note_create" | "ticket_create" => code == 201,
        // A concurrent edit of the same ticket is an expected 409, not a server error.
        "transition" => code == 200 || code == 409,
        _ => code == 200,
    }
}

async fn scenario(
    name: &str,
    c: &reqwest::Client,
    base: &str,
    u: &User,
    ws: &Ws,
    rng: &mut Rng,
    cursor: &mut Option<String>,
) -> Result<u16, String> {
    let wsb = format!("{base}/api/v1/workspaces/{}", ws.id);
    let get = |url: String| c.get(url).header(reqwest::header::COOKIE, &u.cookie);
    let post = |url: String, body: Value, key: String| {
        c.post(url)
            .header(reqwest::header::COOKIE, &u.cookie)
            .header("x-csrf-token", &u.csrf)
            .header("idempotency-key", key)
            .json(&body)
    };
    let key = || format!("perf-{:016x}", rng_key());
    let req = match name {
        "list_default" => get(format!("{wsb}/tickets")),
        "list_next_page" => match cursor.take() {
            Some(cur) => get(format!("{wsb}/tickets")).query(&[("cursor", cur.as_str())]),
            None => get(format!("{wsb}/tickets")),
        },
        "list_active_mine" => get(format!("{wsb}/tickets")).query(&[
            ("status", "new,open,pending_customer,pending_engineering"),
            ("owner", &u.id.to_string()),
            ("sort", "received_at"),
            ("order", "asc"),
        ]),
        "list_filtered_range" => get(format!("{wsb}/tickets")).query(&[
            ("priority", ws.priority.to_string().as_str()),
            ("receivedFrom", &rfc3339_days_ago(30)),
            ("status", "resolved"),
        ]),
        "search_text" => {
            let w = SEARCH_WORDS[rng.below(SEARCH_WORDS.len())];
            get(format!("{wsb}/tickets")).query(&[("q", w)])
        }
        "search_number" => {
            let n = ws.sample[rng.below(ws.sample.len())].1;
            get(format!("{wsb}/tickets")).query(&[("q", n.to_string())])
        }
        "board" => get(format!("{wsb}/tickets/board")),
        "dashboard" => {
            let period = ["today", "last_7_days", "this_month"][rng.below(3)];
            get(format!("{wsb}/dashboard")).query(&[("period", period)])
        }
        "detail" => get(format!(
            "{wsb}/tickets/{}/detail",
            ws.sample[rng.below(ws.sample.len())].0
        )),
        "detail_hot" if !ws.hot.is_empty() => get(format!(
            "{wsb}/tickets/{}/detail",
            ws.hot[rng.below(ws.hot.len())]
        )),
        "detail_hot" => get(format!("{wsb}/tickets/{}/detail", ws.sample[0].0)),
        "notes" => get(format!(
            "{wsb}/tickets/{}/notes",
            ws.sample[rng.below(ws.sample.len())].0
        )),
        "activity" => get(format!(
            "{wsb}/tickets/{}/activity",
            ws.sample[rng.below(ws.sample.len())].0
        )),
        "note_create" => {
            let t = ws.active[rng.below(ws.active.len())];
            post(
                format!("{wsb}/tickets/{t}/notes"),
                json!({"body": "Catatan uji beban (dummy)."}),
                key(),
            )
        }
        "transition" => {
            // Read the current version and status first, as the UI does.
            let t = ws.active[rng.below(ws.active.len())];
            let r = get(format!("{wsb}/tickets/{t}"))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if r.status().as_u16() != 200 {
                return Err(format!("get before transition: {}", r.status()));
            }
            let v: Value = r.json().await.map_err(|e| e.to_string())?;
            let tk = v.get("ticket").unwrap_or(&v);
            let version = tk["version"].as_i64().unwrap_or(1);
            let status = tk["status"]["code"].as_str().unwrap_or("open");
            let to = if status == "open" {
                "pending_customer"
            } else {
                "open"
            };
            post(
                format!("{wsb}/tickets/{t}/transitions"),
                json!({"to": to, "version": version}),
                key(),
            )
        }
        "ticket_create" => post(
            format!("{wsb}/tickets"),
            json!({
                "subject": "Tiket uji beban (dummy)",
                "chronology": "Kronologi uji beban (dummy).",
                "receivedAt": rfc3339_days_ago(0),
                "intakeChannelId": ws.channel,
                "priorityId": ws.priority,
            }),
            key(),
        ),
        other => return Err(format!("skenario tidak dikenal: {other}")),
    };
    let r = req.send().await.map_err(|e| e.to_string())?;
    let code = r.status().as_u16();
    let body: Value = r.json().await.unwrap_or(Value::Null);
    // Only the unfiltered list hands its cursor on: a cursor is bound to the filters of the
    // list that issued it, and `list_next_page` sends none.
    if matches!(name, "list_default" | "list_next_page") {
        *cursor = body["nextCursor"].as_str().map(str::to_owned);
    }
    if ok_code(name, code) {
        Ok(code)
    } else {
        Err(format!("{name}: HTTP {code}"))
    }
}

fn rng_key() -> u64 {
    let mut b = [0u8; 8];
    // Unique per request; falls back to the clock if the OS source is unavailable.
    if getrandom::fill(&mut b).is_err() {
        return u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
                % u128::from(u64::MAX),
        )
        .unwrap_or(0);
    }
    u64::from_le_bytes(b)
}

fn rfc3339_days_ago(days: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .saturating_sub(days * 86_400 + 60);
    let (d, s) = (now / 86_400, now % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = i64::try_from(d).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        (s % 3600) / 60,
        s % 60
    )
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    // Nearest-rank percentile.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1]
}

fn report(
    stats: &[(&&'static str, &Stat)],
    secs: f64,
    users: i64,
    think: u64,
    out: Option<&str>,
) -> Res<()> {
    let mut rows = Vec::new();
    let mut by_class: BTreeMap<Class, Vec<f64>> = BTreeMap::new();
    let mut total = 0u64;
    let mut failures = 0u64;
    println!(
        "| skenario | kelas | n | p50 ms | p95 ms | p99 ms | maks ms | target p95 | status | gagal |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for (name, class, _) in MIX {
        let Some((_, st)) = stats.iter().find(|(n, _)| **n == *name) else {
            continue;
        };
        let mut v = st.ms.clone();
        v.sort_by(f64::total_cmp);
        by_class.entry(*class).or_default().extend(&v);
        let n = v.len() as u64;
        total += n;
        failures += st.errors;
        let (p50, p95, p99) = (pct(&v, 50.0), pct(&v, 95.0), pct(&v, 99.0));
        let max = v.last().copied().unwrap_or(0.0);
        let codes = st
            .statuses
            .iter()
            .map(|(c, n)| format!("{c}×{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        let fails = st
            .failures
            .iter()
            .map(|(r, n)| format!("{r}×{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "| {name} | {} | {n} | {p50:.0} | {p95:.0} | {p99:.0} | {max:.0} | ≤{:.0} {} | {codes} | {} {fails} |",
            class.name(),
            class.target_ms(),
            if p95 <= class.target_ms() {
                "LULUS"
            } else {
                "GAGAL"
            },
            st.errors
        );
        rows.push(
            json!({"scenario": name, "class": class.name(), "n": n, "p50": p50, "p95": p95,
            "p99": p99, "max": max, "target_p95": class.target_ms(), "statuses": codes,
            "errors": st.errors, "failures": st.failures}),
        );
    }
    println!();
    println!("| kelas | n | p95 ms | target | hasil |");
    println!("|---|---|---|---|---|");
    let mut classes = Vec::new();
    let mut all_pass = failures == 0;
    for (class, v) in &mut by_class {
        v.sort_by(f64::total_cmp);
        let p95 = pct(v, 95.0);
        let pass = p95 <= class.target_ms();
        all_pass &= pass;
        println!(
            "| {} | {} | {p95:.0} | ≤{:.0} | {} |",
            class.name(),
            v.len(),
            class.target_ms(),
            if pass { "LULUS" } else { "GAGAL" }
        );
        classes.push(json!({"class": class.name(), "n": v.len(), "p95": p95,
            "target_p95": class.target_ms(), "pass": pass}));
    }
    #[allow(clippy::cast_precision_loss)]
    let rps = total as f64 / secs;
    println!();
    println!(
        "total {total} request dalam {secs:.0} dtk ({rps:.1} req/dtk), {users} pengguna, jeda {think} ms, gagal {failures}"
    );
    println!("hasil: {}", if all_pass { "LULUS" } else { "GAGAL" });
    if let Some(path) = out {
        let doc = json!({"users": users, "think_ms": think, "seconds": secs, "requests": total,
            "rps": rps, "failures": failures, "pass": all_pass, "scenarios": rows,
            "classes": classes});
        std::fs::write(
            path,
            serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("tulis {path}: {e}"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- stats

async fn stats(o: &Opts) -> Res<()> {
    let top = o.num("top", 15, 1, 100)?;
    let db = pool(1).await?;
    let e = |e: sqlx::Error| e.to_string();
    sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
        .execute(&db)
        .await
        .map_err(e)?;
    if o.text("reset").is_some() {
        sqlx::query("SELECT pg_stat_statements_reset()")
            .execute(&db)
            .await
            .map_err(e)?;
        println!("pg_stat_statements direset");
        return Ok(());
    }
    let rows = sqlx::query(
        "SELECT calls, round(total_exec_time)::bigint AS total_ms, \
           round(mean_exec_time::numeric, 2)::float8 AS mean_ms, \
           round(max_exec_time::numeric, 1)::float8 AS max_ms, rows, \
           left(regexp_replace(query, '\\s+', ' ', 'g'), 220) AS q \
         FROM pg_stat_statements WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) \
         ORDER BY total_exec_time DESC LIMIT $1",
    )
    .bind(top)
    .fetch_all(&db)
    .await
    .map_err(e)?;
    println!("| calls | total ms | mean ms | max ms | rows | query |");
    println!("|---|---|---|---|---|---|");
    for r in rows {
        let q: String = r.get("q");
        println!(
            "| {} | {} | {} | {} | {} | `{}` |",
            r.get::<i64, _>("calls"),
            r.get::<i64, _>("total_ms"),
            r.get::<f64, _>("mean_ms"),
            r.get::<f64, _>("max_ms"),
            r.get::<i64, _>("rows"),
            q.replace('|', "\\|")
        );
    }
    Ok(())
}
