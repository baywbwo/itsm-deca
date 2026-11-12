# Runbook: observability API (CP7a)

## Ringkasan
- **Log**: satu baris JSON per kejadian ke stdout (`docker compose logs api`). Setiap request menghasilkan satu baris
  `request done` di span `request{request_id, method, route}` dengan `status` dan `latency_ms`.
  - `route` adalah template Axum (mis. `/api/v1/workspaces/{workspace_id}/tickets`), bukan URI. Path asli, query string,
    cookie, dan body tidak pernah ditulis. Path yang tidak dikenal dicatat sebagai `unmatched`.
  - Level: `/health/*` di DEBUG, 5xx di WARN, sisanya INFO. Atur dengan `RUST_LOG` (default `info`).
  - Error database dicatat dengan teks `Display` saja; detail baris dari Postgres (`detail`) tidak ikut.
- **Request id**: header `x-request-id` dari klien dipakai hanya bila 1–64 karakter `[A-Za-z0-9._:-]` dan hanya satu.
  Selain itu diganti UUID baru. Nilainya dikembalikan di response, jadi pengguna bisa melaporkannya ke support dan
  support mencarinya di log (`grep '"request_id":"<id>"'`).
- **Metrik**: listener terpisah `ITSM_METRICS_BIND` (UAT: `0.0.0.0:9464`, `GET /metrics`, format teks Prometheus 0.0.4).
  - Tidak ada port yang dipublikasikan, dan Caddy hanya meneruskan `/api/*` dan `/health/*`. Akibatnya `/metrics` hanya
    terjangkau dari jaringan `backend`.
  - Router API utama menjawab 404 untuk `/metrics`.
  - Kosongkan `ITSM_METRICS_BIND` untuk mematikan listener. Nilai yang sama dengan `ITSM_BIND` ditolak saat start.
- **Probe**:
  - `/health/live` memeriksa proses saja, tanpa database. Dipakai untuk restart.
  - `/health/ready` memeriksa database dan guard runtime (env database cocok) dengan timeout 2 detik. Jawabannya 503
    `not_ready` bila gagal.
    Dipakai untuk healthcheck compose (`api-healthcheck.sh`) dan load balancer.
- **Shutdown**: SIGTERM/SIGINT menulis log `shutdown requested`, server berhenti menerima koneksi, request yang sedang
  berjalan diselesaikan, lalu worker outbox dan listener metrik berhenti. `docker compose stop` (grace 10 detik) cukup.
- **Worker terpisah (CP7b)**: service `worker` menjalankan worker outbox tanpa API dan mengekspor metrik yang sama di
  `worker:9464`. Topologi, ukuran pool, dan alert ItsmWorkerDown/ItsmOutboxNoWorker ada di `deploy/RUNBOOK-scaling.md`.

## Metrik
| Nama | Jenis | Label | Arti |
|---|---|---|---|
| `itsm_build_info` | gauge | `version` | Versi build yang berjalan |
| `itsm_process_start_time_seconds` | gauge | – | Waktu start proses (deteksi restart) |
| `itsm_http_requests_total` | counter | `method`, `route`, `status` | Request per template route |
| `itsm_http_request_duration_seconds` | histogram | `method`, `route` | Latensi. Bucket 0.05/0.1/0.3/0.6/1/1.5/3/10 detik mengikuti target p95 |
| `itsm_db_pool_connections` | gauge | `state` (`idle`, `in_use`) | Isi pool database |
| `itsm_db_pool_max_connections` | gauge | – | Ukuran pool |
| `itsm_outbox_worker_enabled` | gauge | – | 1 bila proses ini menjalankan worker (`ITSM_OUTBOX_WORKER`) |
| `itsm_outbox_worker_last_tick_timestamp_seconds` | gauge | – | Waktu tick worker terakhir yang sukses (0 = belum ada) |
| `itsm_outbox_worker_tick_failures_total` | counter | – | Tick yang gagal (database tidak tersedia, dll.) |
| `itsm_outbox_intents` | gauge | `provider` (`jira`, `lark`), `state` (`pending`, `failed`, `unknown`) | Intent yang belum terkirim |
| `itsm_outbox_oldest_age_seconds` | gauge | `provider`, `state` | Umur intent tertua (0 = tidak ada) |
| `itsm_metrics_scrape_errors_total` | counter | – | Scrape yang gagal membaca metrik dari database |

Catatan:
- Gauge outbox dibaca dari database lewat indeks parsial `integration_outbox_open_idx` (migrasi 0028), dengan timeout 2 detik.
  Nilainya sama di setiap replika, jadi agregasikan dengan `max`, bukan `sum`.
- Bila database tidak tersedia, scrape tetap 200 tanpa gauge outbox, dan `itsm_metrics_scrape_errors_total` naik.
- Label tidak pernah memuat id, nama pengguna, atau isi request.

## Aturan alert (`deploy/observability/alerts.yml`)
Scrape job harus bernama `itsm-api` dengan target `api:9464`, dan `itsm-worker` dengan satu target `:9464` per replika worker. Layanan Prometheus dan Alertmanager masuk CP8. Sampai saat itu,
file ini menjadi kontrak, dan `api/tests/observability.rs` memastikan setiap metrik yang dipakai aturan memang diekspor.

### ItsmApiDown
Endpoint metrik tidak terjangkau selama 2 menit.
1. `docker compose ps api` dan `docker compose logs --tail=200 api`. Cari `shutdown requested`, panic, atau error konfigurasi saat start.
2. Bila container berulang kali restart, periksa `/health/live` dari dalam jaringan dan konfigurasi env (`check-config.sh`).
3. Rollback ke image tag SHA sebelumnya bila gangguan muncul setelah rilis.

### ItsmApiRestarting
Lebih dari 2 restart dalam 30 menit. Biasanya OOM, panic, atau healthcheck gagal karena database.
Periksa `docker compose logs api` dan `docker inspect` (OOMKilled), lalu lanjutkan seperti ItsmApiDown.

### ItsmHttp5xxRate
Lebih dari 2% request menjawab 5xx selama 10 menit, dengan trafik di atas 0,05 request/detik.
1. Cari baris WARN `request done` dengan `status` 5xx, lalu kelompokkan per `route`.
2. Ikuti `request_id` dari baris tersebut ke log error di span yang sama (`error.kind`).
3. 503 `not_ready` atau error pool biasanya berarti database. Lihat ItsmDbPoolSaturated dan `/health/ready`.

### ItsmLatency
Tiga aturan untuk p95 per route selama 15 menit:
- `ItsmLatencyReadP95`: GET detail di atas 300 ms.
- `ItsmLatencyWriteP95`: tulis di atas 600 ms.
- `ItsmLatencyListP95`: GET daftar/laporan di atas 1,5 detik.

Langkah:
1. Lihat route di label alert. Untuk daftar, periksa filter yang dipakai pengguna. `q` tidak dicatat, jadi tanyakan
   langsung ke pengguna bila perlu.
2. Periksa pool (`itsm_db_pool_connections`) dan beban Postgres (`pg_stat_activity`, query lambat).
3. Laporan/ekspor besar dibatasi `ITSM_REPORT_MAX_ROWS`. Turunkan nilainya bila ekspor membebani.

### ItsmOutboxBacklogOld
Intent `pending` tertua sudah lebih dari 10 menit.
1. Pastikan worker berjalan (lihat ItsmOutboxWorkerStalled dan ItsmOutboxNoWorker): service `worker` ≥ 1 replika, atau
   `ITSM_OUTBOX_WORKER=on` di `api`.
2. Periksa log worker (`outbox`) dan egress proxy (`docker compose logs egress-proxy`) untuk 403/407, DNS, atau timeout penyedia.
3. Periksa status Jira/Lark. Kegagalan transient dicoba ulang otomatis dalam batas percobaan, dan tidak ada intent yang hilang.

### ItsmOutboxFailed
Ada intent baru berstatus `failed` dalam 15 menit. Worker tidak mencobanya lagi.
1. Buka panel sinkron Jira/Lark di detail tiket untuk melihat kalimat error tetap per intent (mis. workflow Jira tanpa
   transisi berarti "Ubah manual di Jira").
2. Perbaiki penyebabnya (mapping status, kredensial yang di-rotate, proyek Jira), lalu tekan **Kirim ulang** (butuh hak
   akses) atau ubah manual di Jira.

### ItsmOutboxUnknown
Ada intent dengan hasil `unknown`, artinya permintaan mungkin sudah sampai ke penyedia tetapi jawabannya hilang. Intent ini
tidak dicoba ulang otomatis agar tidak terjadi duplikasi.
1. Cek di Jira/Lark apakah issue, komentar, atau pesan sudah ada.
2. Di panel sinkron detail tiket, pilih **Rekonsiliasi dengan bukti**. Catat hasilnya (sudah ada atau belum) beserta buktinya.

### ItsmOutboxWorkerStalled
Worker aktif tetapi tidak menyelesaikan tick selama lebih dari 2 menit.
1. Lihat `itsm_outbox_worker_tick_failures_total`. Bila nilainya naik, database tidak tersedia (lihat `/health/ready`).
2. Bila tidak naik, worker mungkin macet di panggilan penyedia. Setiap panggilan dibatasi `ITSM_INTEGRATIONS_TIMEOUT_MS`.
   Restart proses di label `instance` (`worker`, atau `api` bila worker berjalan di sana) aman karena intent tetap di
   database.

### ItsmDbPoolSaturated
Lebih dari 90% koneksi pool terpakai selama 10 menit.
1. Periksa `pg_stat_activity` untuk query panjang atau lock.
2. Periksa lonjakan trafik per route di `itsm_http_requests_total`.
3. Naikkan ukuran pool hanya setelah penyebabnya diketahui.

### ItsmMetricsDatabaseUnreadable
Scrape gagal membaca gauge outbox dari database. Penyebabnya database tidak tersedia, atau query melewati timeout 2 detik.
Periksa `/health/ready` dan Postgres. Selama alert ini aktif, alert outbox tidak bisa menyala.

## Belum tercakup (CP8)
- Layanan Prometheus dan Alertmanager di compose, routing notifikasi, dan validasi `promtool check rules`.
- Tracing terdistribusi (OTLP). Korelasi saat ini memakai `request_id` di log.
