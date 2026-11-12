# Runbook: worker terpisah dan skala (CP7b)

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
- outbox.

Sisanya bersifat lokal per proses:

| State | Sifat | Dampak multi-replika |
|---|---|---|
| Rate limiter (`rate_limit.rs`) | Per proses | Batas efektif = batas × jumlah replika API. Penyelarasan antar replika: CP7d |
| Cache token tenant Lark (`live.rs`) | Cache | Tiap proses meminta token sendiri; tidak ada ketidakkonsistenan |
| Cache discovery/JWKS OIDC (`oidc.rs`, TTL 1 jam) | Cache | Setelah rotasi kunci IdP, replika lain memuat ulang saat `kid` tidak dikenal atau TTL habis |
| Lampiran (`ITSM_ATTACHMENT_DIR`) | Volume `attachments` | Fitur mati sejak D13. Volume hanya bisa dibagi di satu host. Untuk multi-host perlu object storage (gap) |
| SLA | Dihitung saat baca (`dashboard.rs`) | Tidak ada job SLA, tidak ada state |

Migrasi memakai advisory lock Postgres (sqlx), jadi dua `api` yang start bersamaan tidak saling menimpa.

## Belum tercakup
- Beberapa replika `api` di belakang Caddy. `reverse_proxy` saat ini satu upstream (`ITSM_UAT_API_UPSTREAM`);
  load balancing dan health upstream masuk CP8.
- Rate limit lintas replika (CP7d).
- Object storage untuk lampiran (bila fitur lampiran dihidupkan lagi).
- Compose dan Docker belum dijalankan di lingkungan ini. Topologi diuji dengan proses lokal (`evidence/i3/cp7b/`).
