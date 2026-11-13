# Runbook: worker terpisah, skala, dan kinerja (CP7b, CP7c)

## Topologi
| Service | Perintah | Isi | Port |
|---|---|---|---|
| `api` | `serve` | Migrasi saat start, lalu HTTP API (`:8080`) dan metrik (`:9464`). Worker outbox mati (`ITSM_OUTBOX_WORKER=off`). | 8080, 9464 (jaringan `backend` saja) |
| `worker` | `worker` | Worker outbox saja: kirim Jira/Lark, sapu lease kedaluwarsa, pangkas event webhook lama. Tanpa listener API dan tanpa migrasi. | 9464 (metrik dan heartbeat) |

- Image sama (`itsm-uat-api:<SHA>`), env sama (anchor `api-env` di `compose.yaml`).
- `worker` menunggu `api` sehat, karena `api` yang menjalankan migrasi. Worker menolak start bila guard runtime
  database tidak cocok, sama seperti API.
- Menjalankan beberapa worker aman: `docker compose up -d --scale worker=3`.
  - Setiap intent diklaim dengan `FOR UPDATE SKIP LOCKED` dan token klaim berlease 30 detik. Worker lain melewati baris
    yang sedang dikunci, jadi satu intent tidak dikirim dua kali.
  - Satu tick mengklaim satu intent pada satu waktu (maksimal 20 per tick). Lease dimulai tepat sebelum percobaannya
    sendiri. Panggilan penyedia dibatasi `ITSM_INTEGRATIONS_TIMEOUT_MS` (maksimal 8 detik), jauh di bawah lease.
  - Klaim yang lease-nya habis tanpa hasil (proses mati di tengah) diubah menjadi `unknown` oleh worker mana pun, dan
    tidak dicoba ulang otomatis. Rekonsiliasi lewat panel sinkron (RUNBOOK-observability, ItsmOutboxUnknown).
  - Bukti: `api/tests/worker.rs` (`cp7b_worker_instances_share_a_backlog_without_double_delivery`,
    `cp7b_worker_tick_holds_one_claim_at_a_time`).
- Kembali ke satu proses (rollback topologi): `ITSM_OUTBOX_WORKER=on` di `.env`, lalu
  `docker compose up -d --scale worker=0`. Jangan jalankan keduanya bersamaan tanpa alasan. Hal itu tetap aman, tetapi
  menambah koneksi database.

## Ukuran pool database
| Env | Default | Batas | Arti |
|---|---|---|---|
| `ITSM_DB_MAX_CONNECTIONS` | 10 | 1–200 | Pool per proses `api` |
| `ITSM_WORKER_DB_MAX_CONNECTIONS` | 4 | 1–200 | Pool per proses `worker` (compose memetakannya ke `ITSM_DB_MAX_CONNECTIONS`) |
| `ITSM_DB_ACQUIRE_TIMEOUT_MS` | 3000 | 100–30000 | Tunggu koneksi bebas. Bila habis, request menjawab 503 dan log `db_unavailable` |

Nilai di luar batas atau bukan angka membuat proses menolak start.

Rumus: `replika_api × ITSM_DB_MAX_CONNECTIONS + replika_worker × ITSM_WORKER_DB_MAX_CONNECTIONS + cadangan ≤ max_connections Postgres`.
- Postgres UAT memakai default `max_connections = 100`.
- Sisakan minimal 10 koneksi untuk admin, `pg_dump`, dan migrasi.
- Contoh: 1 api × 10 + 3 worker × 4 = 22 koneksi.

Worker memakai paling banyak 2 koneksi bersamaan per tick (klaim, lalu settle). 4 sudah longgar.

Memeriksa pemakaian (nama aplikasi diset per proses):
```sql
SELECT application_name, state, count(*) FROM pg_stat_activity
WHERE datname = current_database() GROUP BY 1, 2 ORDER BY 1, 2;
```
`itsm-api` adalah proses API, `itsm-worker` proses worker. Metrik pool per proses ada di `itsm_db_pool_connections` dan
`itsm_db_pool_max_connections` (alert ItsmDbPoolSaturated).

## Health dan alert
- Healthcheck `worker` (`worker-healthcheck.sh`) membaca `/metrics` worker sendiri. Container sehat bila tick terakhir
  yang sukses berumur ≤ 120 detik (`ITSM_WORKER_STALE_SECS`).
  - Tick yang gagal (database tidak tersedia) tidak memperbarui heartbeat, sehingga container menjadi `unhealthy`.
  - Compose tidak me-restart container `unhealthy`; itu tugas alert dan operator.
- Prometheus: tambahkan job `itsm-worker` dengan satu target per replika worker (port 9464), di samping job `itsm-api`.
  Layanan Prometheus sendiri masuk CP8.

### ItsmWorkerDown
Endpoint metrik worker tidak terjangkau selama 2 menit.
1. `docker compose ps worker` dan `docker compose logs --tail=200 worker`. Cari error konfigurasi, guard database,
   atau `database connect failed`.
2. Intent tetap aman di database. Selama ada worker lain yang sehat, backlog tetap dikirim.
3. Restart: `docker compose up -d worker`. Rollback image seperti `api` bila muncul setelah rilis.

### ItsmOutboxNoWorker
Tidak ada proses yang menjalankan worker outbox selama 5 menit, sehingga intent Jira/Lark menumpuk (ItsmOutboxBacklogOld
akan menyusul).
1. Pastikan service `worker` berjalan dengan `--scale worker` ≥ 1, atau `ITSM_OUTBOX_WORKER=on` di `api`.
2. Bila worker berjalan tetapi tidak terscrape, periksa target Prometheus job `itsm-worker`.

## State di dalam proses
Semua state yang harus konsisten antar replika ada di Postgres:
- sesi;
- percobaan login OIDC;
- idempotensi webhook dan intake;
- outbox;
- jendela rate limit bersama (`rate_limit_windows`, CP7d).

Sisanya bersifat lokal per proses:

| State | Sifat | Dampak multi-replika |
|---|---|---|
| Rate limiter umum (`rate_limit.rs`) | Per proses | Batas efektif = batas × jumlah replika API. Sengaja: lapis pertama yang murah untuk semua lalu lintas. Endpoint mahal atau rawan dihitung bersama di Postgres (lihat di bawah) |
| Cache token tenant Lark (`live.rs`) | Cache | Tiap proses meminta token sendiri; tidak ada ketidakkonsistenan |
| Cache discovery/JWKS OIDC (`oidc.rs`, TTL 1 jam) | Cache | Setelah rotasi kunci IdP, replika lain memuat ulang saat `kid` tidak dikenal atau TTL habis |
| Lampiran (`ITSM_ATTACHMENT_DIR`) | Volume `attachments` | Fitur mati sejak D13. Volume hanya bisa dibagi di satu host. Untuk multi-host perlu object storage (gap) |
| SLA | Dihitung saat baca (`dashboard.rs`) | Tidak ada job SLA, tidak ada state |

## Rate limit bersama (CP7d)
Endpoint yang mahal atau rawan diserang dihitung di tabel Postgres `rate_limit_windows`, sehingga batasnya sama
berapa pun jumlah replika API. Ini lapis kedua, di belakang rate limiter per proses.

| Kelas | Kunci | Jendela | Default | Variabel |
|---|---|---|---|---|
| Masuk (`POST /login`, `/login/mfa`, `/uat/session`, OIDC start/callback) | Alamat klien | 10 menit | 100 | `ITSM_LIMIT_AUTH_PER_IP_10MIN` |
| Saran AI, per pengguna | Pengguna | 1 menit | 10 | `ITSM_LIMIT_AI_PER_USER_MIN` |
| Saran AI, per perusahaan | Workspace | 1 hari (00.00 UTC = 07.00 WIB) | 2000 | `ITSM_LIMIT_AI_PER_WORKSPACE_DAY` |
| Panggilan keluar (uji koneksi, sinkron direktori, retry outbox, uji penyedia AI/MCP) | Pengguna | 10 menit | 30 | `ITSM_LIMIT_OUTBOUND_PER_USER_10MIN` |
| Lookup Jira langsung (opsi, pencarian pengguna Jira) | Pengguna | 1 menit | 60 | `ITSM_LIMIT_LOOKUP_PER_USER_MIN` |
| Webhook Jira | Alamat klien | 1 menit | 600 | `ITSM_LIMIT_WEBHOOK_PER_IP_MIN` |

Semua nilai 1–1000000; nilai di luar rentang menolak start. Jawaban saat batas terlampaui: `429`,
`{"error":{"code":"rate_limited","retryAfterSeconds":N}}` dan header `Retry-After`. Log
`shared rate limit` dengan `rate_limit.class` (tanpa alamat atau ID pengguna).

Cara kerja dan batasannya:
- Satu `INSERT … ON CONFLICT DO UPDATE` per hit. Hitungan tepat antar replika (diuji paralel dua instance).
- Jendela tetap. Di batas jendela, sampai 2× batas bisa lolos dalam waktu singkat.
- Identitas disimpan sebagai SHA-256 (cakupan + jenis + nilai), bukan alamat atau ID mentah.
- Tabel `UNLOGGED`: setelah crash Postgres hitungan kembali nol. Ini disengaja (data sementara, tanpa beban WAL),
  dan tabel tidak ikut replika baca.
- Worker menghapus jendela lebih tua dari 2 hari, maksimal 5000 baris per putaran.
- Kuota AI dihitung setelah otorisasi dan validasi. Pengguna lain tidak bisa menghabiskan kuota perusahaan lain,
  dan pengguna yang sudah melewati batas pribadinya tidak lagi memakai kuota perusahaan.
- Webhook dihitung per alamat, bukan per koneksi, agar pengirim palsu tidak bisa memblokir pengiriman Jira yang sah.
  Body di atas 1 MiB ditolak `413` sebelum dihitung.
- Perlindungan per akun untuk kata sandi dan kode MFA tetap lockout di baris kredensial (5 gagal, 15 menit).
  Lockout itu sudah di Postgres, jadi berlaku lintas replika.
- Alamat klien hanya dari `ITSM_TRUSTED_PROXIES`. `X-Forwarded-For` dari peer lain diabaikan.

Bila pengguna sah terkena 429 (misalnya satu NAT kantor untuk banyak CS saat jam masuk), naikkan
`ITSM_LIMIT_AUTH_PER_IP_10MIN` lalu restart API. Kosongkan jendela yang sedang berjalan bila perlu:
`DELETE FROM rate_limit_windows WHERE scope = 'auth_ip';` (role migrator).

Migrasi memakai advisory lock Postgres (sqlx), jadi dua `api` yang start bersamaan tidak saling menimpa.

## Kinerja dan volume (CP7c)
Target: p95 baca ≤ 300 ms, tulis ≤ 600 ms, daftar/laporan ≤ 1,5 detik, pada 300 pengguna bersamaan dengan 1 juta
tiket dan sekitar 10 juta catatan/event. Bukti di `evidence/i3/cp7c/`.

### Hasil uji beban
Kondisi uji: 1 proses `api` dengan pool 10, Postgres 18.6 di host 8 core yang sama dengan generator beban, data
dummy, 300 pengguna, jeda 1 detik, pengukuran 180 detik.

| | Sebelum (2b6bcc9) | Sesudah (CP7c) |
|---|---|---|
| p95 baca | 5972 ms | 94 ms |
| p95 daftar/cari/papan/dasbor | 7071 ms | 213 ms (dasbor 350 ms) |
| p95 tulis | 6902 ms | 132 ms |
| Throughput | 49 req/dtk | 286 req/dtk |
| Gagal | 7414 (non-2xx; log server mencatat `db_unavailable` 8662×, yaitu pool habis) | 0 |

Yang mengubah hasil:
- Index yang mengikuti ORDER BY daftar (0029).
- Penghitung status per workspace (0030), sehingga hitungan tanpa filter tidak lagi membaca seluruh index status.
- Dasbor:
  - event resolve hanya dicek ke tiket bila ada filter;
  - "perlu perhatian" dibaca per prioritas lewat index parsial;
  - plan custom per request.

Pool 10 cukup untuk beban ini, dan database tetap menjadi batas CPU. Menaikkan pool tidak menambah throughput bila CPU
Postgres sudah penuh. Pakai rumus pool di atas, lalu ukur ulang di host produksi.

### Jendela migrasi 0029 dan 0030
Diukur pada 1 juta tiket:

| Migrasi | Isi | Waktu | Kunci |
|---|---|---|---|
| 0029 | Membangun ulang 2 index, 1 index baru, `pg_trgm` + index GIN subjek | ±10 detik | SHARE pada `tickets`: baca jalan, tulis menunggu |
| 0030 | Tabel penghitung, backfill, pemicu, index parsial "perlu perhatian" | < 1 detik | SHARE ROW EXCLUSIVE pada `tickets`: baca jalan, tulis menunggu |

Hal yang perlu disiapkan:
- Migrasi memakai `SET LOCAL statement_timeout = 0`, karena pool aplikasi membatasi statement 5 detik.
- Jalankan di luar jam sibuk. Tulis tiket menunggu sampai migrasi selesai, dan request tulis bisa kena `lock_timeout`
  3 detik.
- `CREATE EXTENSION pg_trgm` butuh hak CREATE pada database untuk peran migrasi. Ekstensi ini bawaan contrib Postgres
  (image resmi sudah memuatnya).
- Down 0029 menghapus `pg_trgm`. Down 0030 menghapus tabel penghitung dan pemicunya. Bila 0030 dipasang ulang, backfill
  menghitung ulang dari tiket.

### Penghitung status (`ticket_status_counts`)
- Diisi oleh pemicu statement-level pada INSERT, UPDATE, dan DELETE `tickets`, dalam transaksi yang sama.
  - Pemicu memakai `ENABLE ALWAYS`, jadi impor dengan `session_replication_role = replica` tetap tercatat.
- Tiap (workspace, status) dibagi 16 shard menurut id tiket. Baris penghitung dikunci dalam urutan kunci, sehingga
  transisi berlawanan arah tidak deadlock (`api/tests/list_indexes.rs`).
- Dipakai hanya untuk daftar, papan, dan dasbor tanpa filter. Lingkup berfilter tetap menghitung tiket.
- TRUNCATE `tickets` tidak didukung, karena FK dan tabel append-only menolaknya.

Cek selisih. Hasil 0 baris berarti sesuai. `itsm-perf verify` menjalankan cek yang sama:
```sql
SELECT * FROM
  (SELECT workspace_id, workflow_status, count(*) AS n FROM tickets GROUP BY 1, 2) t
FULL JOIN
  (SELECT workspace_id, workflow_status, sum(n)::bigint AS n FROM ticket_status_counts
   GROUP BY 1, 2 HAVING sum(n) <> 0) c
USING (workspace_id, workflow_status) WHERE t.n IS DISTINCT FROM c.n;
```

Hitung ulang bila ada selisih, dalam satu transaksi, dengan tulis tiket tertahan sebentar:
```sql
BEGIN;
LOCK TABLE tickets IN SHARE ROW EXCLUSIVE MODE;
DELETE FROM ticket_status_counts;
INSERT INTO ticket_status_counts (workspace_id, workflow_status, shard, n)
SELECT workspace_id, workflow_status, hashtext(id::text) & 15, count(*) FROM tickets GROUP BY 1, 2, 3;
COMMIT;
```

### Alat uji (`itsm-perf`, tidak masuk image)
Hanya untuk database dummy bermarker (`ITSM_SEED_CONFIRM_DB`). `DATABASE_URL` diambil dari env dan tidak dicetak.

| Perintah | Kegunaan |
|---|---|
| `itsm-perf seed [--tickets N] [--notes-per-ticket N] [--users N]` | Isi volume. Default 1 juta tiket, sekitar 5 menit di host uji |
| `itsm-perf load --base URL --users 300 --duration-secs 180 --out hasil.json` | Uji beban dengan p50/p95/p99 per skenario dan per kelas terhadap target |
| `itsm-perf verify` | Cek invarian data, termasuk selisih penghitung status. Jalankan setelah uji beban |
| `itsm-perf stats [--top N] [--reset]` | Statement terlambat dari `pg_stat_statements` (bila ekstensi dimuat) |

## Belum tercakup
- Beberapa replika `api` di belakang Caddy. `reverse_proxy` saat ini satu upstream (`ITSM_UAT_API_UPSTREAM`);
  load balancing dan health upstream masuk CP8.
- Object storage untuk lampiran (bila fitur lampiran dihidupkan lagi).
- Compose dan Docker belum dijalankan di lingkungan ini. Topologi diuji dengan proses lokal (`evidence/i3/cp7b/`).
- Kinerja (CP7c):
  - generator beban berjalan di host yang sama dengan Postgres dan API (8 core), sehingga angka di host produksi bisa
    berbeda;
  - data sintetis;
  - Docker belum dipakai untuk uji beban.
- Pencarian teks dengan istilah yang cocok ke puluhan ribu tiket masih menghitung semua kecocokan (rata-rata ±70 ms per
  hitungan saat beban). Istilah kurang dari 3 huruf tidak bisa memakai index trigram.
