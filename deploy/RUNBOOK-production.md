# Runbook: production (CP8)

Stack: `deploy/production/compose.yaml`. Isinya postgres, migrate (one-off), api-a, api-b, worker,
backup, egress-proxy, dan ingress. Jalankan semua perintah dari `deploy/production`.
Data nyata hanya ada di sini. Seed UAT, `uat_superadmin`, dan basic auth ingress tidak dipakai.

| peran DB | dipakai oleh | boleh |
|---|---|---|
| `postgres` (superuser bootstrap) | init cluster, `set-passwords.sh` | hanya lewat socket di dalam container (`pg_hba`: peer) |
| `itsm_migrator` | `migrate`, `init-password`, `reseal` | pemilik database dan semua tabel (DDL) |
| `itsm_app` | api-a, api-b, worker | DML saja: tanpa DDL/TRUNCATE, tanpa UPDATE/DELETE pada tabel append-only, baca-saja `_sqlx_migrations` dan marker |
| `itsm_backup` | layanan backup | REPLICATION saja (base backup), tanpa CONNECT ke database |

API dan worker menolak start bila ada migrasi yang belum diterapkan, gagal, atau checksum-nya beda
("skema database belum dimigrasi …"). Migrasi baru dari rilis berikutnya tetap diterima (fase expand).

## 1. Deploy pertama

1. **Host.** Docker Engine dengan compose v2, jam host tersinkron NTP, dan disk cukup untuk `pgdata` + `backup`
   (lihat "Kapasitas backup"). Hanya port 80/443 yang dibuka ke jaringan kantor/VPN.
2. **Secret** (jangan dicetak ke chat, tiket, atau log):
   ```sh
   install -d -m 700 secrets
   for s in pg_superuser_password pg_migrator_password pg_app_password pg_backup_password; do
     openssl rand -hex 32 > secrets/$s; done
   printf 'k2026a:%s' "$(openssl rand -base64 32)" > secrets/itsm_keyring   # ITSM_KEYRING_ACTIVE=k2026a
   : > secrets/mfa_recovery_pepper        # atau openssl rand -base64 32 bila ITSM_MFA=on
   # egress_proxy_auth + egress_proxy_htpasswd: RUNBOOK-egress.md
   chmod 444 secrets/*
   ```
   Empat password Postgres harus berbeda. `check-config.sh` menolak bila ada yang sama.
3. **Kunci backup (age).** Buat di workstation aman, **bukan** di host database:
   ```sh
   age-keygen -o itsm-backup-identity.txt        # kunci PRIVAT: simpan di vault, 2 salinan
   age-keygen -y itsm-backup-identity.txt        # kunci publik age1… -> deploy/production/backup-recipients
   ```
   Host hanya memegang kunci publik, jadi backup yang dicuri dari host tidak bisa dibaca.
   Tanpa kunci privat, **semua backup tidak bisa dipulihkan**. Boleh beberapa baris `age1…` (misalnya kunci
   owner + kunci tim ops); salah satunya cukup untuk restore.
4. **TLS.**
   - `ITSM_PROD_TLS=file`: sertifikat dari CA perusahaan di `tls/cert.pem` + `tls/key.pem`
     (direktori `tls/` 700, key 400/440/444).
   - Alternatifnya alamat e-mail ACME (butuh DNS publik dan port 80/443 terjangkau CA), atau `internal`
     (CA Caddy; root-nya harus dipasang di setiap klien).
5. **`.env`.** Salin `env.example` ke `.env`, lalu isi `ITSM_PROD_IMAGE_TAG` (SHA rilis yang lulus CI), hostname,
   TLS, dan `ITSM_PROD_ALLOWED_CIDRS`. `ITSM_PROD_PUBLIC_ACCESS=on` (internet) adalah **keputusan owner**.
6. **Pre-flight:** `./check-config.sh` harus `OK`.
7. **Build:** `docker compose build`. Tag image = `ITSM_PROD_IMAGE_TAG`, dan semua base image di-pin digest.
8. **Database:** `docker compose up -d postgres`.
   - Pada volume kosong, `init-db.sh` membuat role (`db/roles.sql`) dan database `itsm` milik `itsm_migrator`,
     menjalankan `db/database.sql`, lalu mengisi password dari `secrets/`.
   - Cek dengan `docker compose logs postgres`. Bila init gagal, hapus volume `pgdata` lalu ulangi;
     image resmi menjalankan init hanya sekali.
9. **Migrasi:** `docker compose run --rm migrate`. Perintah ini berjalan sebagai `itsm_migrator` dan exit 0.
10. **Admin pertama** (password dari file, tidak lewat argumen atau env):
    ```sh
    docker compose run --rm -T -e ITSM_ADMIN_USERNAME=admin.deca -e ITSM_INIT_CONFIRM_DB=itsm \
      migrate init-password < /path/aman/admin-password.txt
    ```
11. **Start:** `docker compose up -d`, lalu `docker compose ps`. Semua layanan harus `healthy`,
    kecuali `migrate` yang `exited (0)`.
12. **Verifikasi:**
    - Buka `https://<host>/`, login, buat tiket uji, lalu hapus atau tutup tiket itu.
    - `docker compose exec -u postgres postgres psql -d itsm -c 'SELECT archived_count, failed_count, last_archived_wal FROM pg_stat_archiver'`
      → `failed_count = 0`.
    - `docker compose exec backup ls /backup/base` → ada `*.tar.age` + `*.meta`. Loop backup langsung membuat base
      backup pertama bila belum ada.

Bila role dibuat belakangan pada database yang sudah ada, jalankan
`docker compose exec -u postgres postgres psql -d itsm -c 'SELECT itsm_grant_app()'` setelah `db/roles.sql`.

## 2. Upgrade (expand/contract, tanpa downtime)

Aturan migrasi: rilis N hanya boleh **menambah** (kolom nullable, tabel, indeks `CONCURRENTLY` bila besar),
sehingga binary N-1 tetap jalan di skema N. Penghapusan atau perubahan yang tidak kompatibel menunggu rilis
N+1 (contract), setelah tidak ada lagi binary N-1 yang berjalan.

1. CI hijau pada SHA rilis. Isi `ITSM_PROD_IMAGE_TAG=<sha>` di `.env`, lalu `./check-config.sh`.
2. `docker compose build`
3. Base backup sebelum migrasi:
   `docker compose run --rm --entrypoint /usr/local/lib/itsm/backup/base-backup.sh backup`
4. `docker compose run --rm migrate`
5. Rolling restart: `docker compose up -d --no-deps api-a`, tunggu `healthy`, lalu `… api-b`, `… worker`, dan `… ingress`.
   - Caddy hanya merutekan ke replika yang sehat (health check aktif 3 s) dan memindahkan request yang gagal
     tersambung ke replika lain.
   - Bukti e2e: SIGTERM pada satu replika di tengah 60 request menghasilkan 0 error; SIGKILL juga 0 error, karena
     baca dan 4 tulis ber-Idempotency-Key diulang dengan aman.
   - Tulis lain yang sedang berjalan di replika yang **crash** bisa mendapat 502, dan pengguna mengulanginya.

**Rollback kode:** kembalikan `ITSM_PROD_IMAGE_TAG` ke SHA sebelumnya, lalu `docker compose up -d --no-deps api-a api-b worker`.
Binary lama menerima skema yang lebih baru (expand), jadi rollback ini tidak menyentuh database.
**Rollback skema** hanya dengan persetujuan owner, dengan dua cara:
- jalankan file `api/migrations/NNNN_*.down.sql` secara manual sebagai `itsm_migrator`;
- PITR ke titik sebelum migrasi (bagian 5).

## 3. Rotasi password database

1. Pilih jendela sepi. Satu role hanya punya satu password, jadi koneksi baru dengan password lama ditolak
   sampai prosesnya di-restart.
2. Tulis password baru **di tempat**, supaya inode yang di-bind-mount tetap sama:
   `chmod 644 secrets/pg_app_password && openssl rand -hex 32 > secrets/pg_app_password && chmod 444 secrets/pg_app_password`
3. `./check-config.sh`, lalu `docker compose exec -u postgres postgres /usr/local/lib/itsm/set-passwords.sh`.
   Skrip ini membaca ketiga file dari `/run/secrets` dan menyetel password SCRAM tanpa mencatatnya ke log.
4. Restart pemakainya: app → `up -d --no-deps --force-recreate api-a api-b worker`; migrator → tidak ada proses
   tetap; backup → `… backup`.

Keyring dan pepper MFA: `RUNBOOK-keyring.md`. Kredensial proxy egress: `RUNBOOK-egress.md`.

## 4. Pemantauan

- Health compose: api-a, api-b, worker, ingress, postgres, dan backup. Layanan backup menjadi `unhealthy` bila tidak
  ada base backup dalam `interval + 2 jam`.
- Metrik dan alert API/worker: `RUNBOOK-observability.md` (`/metrics` di port 9464, jaringan internal saja).
- Arsip WAL: `pg_stat_archiver.failed_count` harus 0, dan `last_archived_time` tidak boleh lebih dari 10 menit
  (`archive_timeout` 5 menit). Arsip yang gagal menumpuk WAL di `pgdata`, dan bila dibiarkan disk penuh
  sehingga database berhenti.
- Disk volume `backup` dan `pgdata`.

## 5. Backup dan PITR

**Isi volume `backup`:**
- `wal/<segmen>.age`: setiap segmen WAL, dienkripsi `archive_command` (`backup/wal-archive.sh`), ditambah
  checksum plaintext di `wal-sums/`;
- `base/<UTC>.tar.age` + `.meta`: base backup harian dari `pg_basebackup` sebagai `itsm_backup`;
- retensi: `ITSM_BACKUP_KEEP` (default 7) base backup terbaru, dan WAL yang lebih tua dari base tertua ikut dipangkas.

Semua data dienkripsi age dengan kunci publik dari `backup-recipients`. Kunci privat tidak pernah ada di host.

**Kapasitas backup:** kira-kira 7 × ukuran database + WAL 7 hari. Pantau tren di minggu pertama.

**Salinan off-site: BELUM ADA (TUNGGU owner).** Volume `backup` ada di host yang sama, jadi kehilangan host berarti
kehilangan backup. Tujuan dan jadwal salinan (misalnya `rsync` atau object storage ke lokasi lain) perlu
diputuskan owner. Karena file sudah terenkripsi, tujuan tidak perlu dipercaya untuk kerahasiaan.

**Restore / PITR** (contoh: kembali ke `2026-11-20 03:15:00+00`; waktu dalam UTC):

1. Ambil kunci privat dari vault ke host, misalnya `/root/itsm-restore/identity` (chmod 400), **hanya selama restore**.
2. `docker compose stop` (volume tetap ada).
3. Simpan data lama:
   ```sh
   docker run --rm -v itsm-prod_pgdata:/from -v itsm-prod_pgdata_before_restore:/to --entrypoint cp \
     itsm-prod-postgres:$TAG -a /from/. /to/
   docker run --rm -v itsm-prod_pgdata:/var/lib/postgresql --entrypoint rm itsm-prod-postgres:$TAG \
     -rf /var/lib/postgresql/18
   ```
4. Pilih base backup terbaru yang **lebih tua** dari waktu target (`ls backup/base`), lalu:
   ```sh
   docker run --rm --user postgres -v itsm-prod_pgdata:/var/lib/postgresql -v itsm-prod_backup:/backup \
     -v /root/itsm-restore:/run/itsm-restore:ro -e ITSM_BACKUP_IDENTITY=/run/itsm-restore/identity \
     itsm-prod-postgres:$TAG /usr/local/lib/itsm/backup/restore.sh \
     /backup/base/<UTC>.tar.age /var/lib/postgresql/18/docker '2026-11-20 03:15:00+00'
   ```
   Kunci salah, file diubah, atau manifest tidak cocok → skrip berhenti dan direktori tujuan dikosongkan.
5. Jalankan recovery dalam container sementara tanpa jaringan:
   ```sh
   docker run -d --name itsm-recovery --network none -v itsm-prod_pgdata:/var/lib/postgresql \
     -v itsm-prod_backup:/backup -v "$PWD/backup-recipients:/etc/itsm/backup/age-recipients:ro" \
     -v /root/itsm-restore:/run/itsm-restore:ro itsm-prod-postgres:$TAG \
     postgres -c config_file=/etc/itsm/postgres/postgresql.conf
   docker logs -f itsm-recovery    # tunggu "database system is ready to accept connections"
   ```
   Segmen WAL yang gagal didekripsi atau checksum-nya salah menghentikan recovery dengan FATAL; lihat log dan
   jangan dipaksa lanjut.
6. Verifikasi, lalu bersihkan setelan recovery:
   ```sh
   docker exec -u postgres itsm-recovery psql -d itsm -c 'SELECT pg_is_in_recovery()'   # f
   docker exec -u postgres itsm-recovery psql -d itsm -c 'SELECT max(created_at) FROM tickets'
   docker exec -u postgres itsm-recovery psql -c 'ALTER SYSTEM RESET restore_command' \
     -c 'ALTER SYSTEM RESET recovery_target_time' -c 'ALTER SYSTEM RESET recovery_target_inclusive' \
     -c 'ALTER SYSTEM RESET recovery_target_action'
   docker stop itsm-recovery && docker rm itsm-recovery
   shred -u /root/itsm-restore/identity
   ```
7. `docker compose up -d postgres`, lalu langsung buat base backup baru (timeline baru):
   `docker compose run --rm --entrypoint /usr/local/lib/itsm/backup/base-backup.sh backup`
8. `docker compose up -d`. `migrate` tidak mengubah apa pun bila skema sama, dan API/worker menolak start bila
   skema hasil restore lebih tua dari binary. Dalam kasus itu, jalankan `migrate` atau pakai tag image yang sesuai.
9. Hapus `itsm-prod_pgdata_before_restore` setelah owner menyetujui hasil restore.

**Latihan restore:** sebulan sekali dan setelah setiap perubahan skema besar. Latih di host terpisah: salin
volume `backup`, lalu ikuti langkah 4–6. Catat waktu restore (RTO) dan selisih data (RPO ≤ 5 menit
dari `archive_timeout`). Latihan otomatis tanpa Docker: `evidence/i3/cp8/backup-drill.sh`.

## 6. Ingress

- `ingress-guard.sh` menolak start tanpa hostname, mode TLS, dan allowlist.
  - `0.0.0.0/0` atau `::/0` hanya diterima dengan `ITSM_PROD_PUBLIC_ACCESS=on`.
  - Prefix lebih lebar dari /8 (IPv4) atau /32 (IPv6) ditolak.
- Klien di luar allowlist mendapat 403 sebelum request diteruskan ke API. API hanya memercayai `X-Forwarded-For`
  dari IP tetap ingress (`ITSM_PROD_INGRESS_IP`).
- Log Caddy (stdout), baik access log maupun error log, menyamarkan Cookie, Set-Cookie, dan Authorization, serta
  menghapus X-CSRF-Token. Error log (misalnya gagal dial ke replika) ikut memuat header request, jadi filter juga
  dipasang di logger global.
- Batas body API ditegakkan juga di ingress: 64 KiB, dan 1 MiB untuk webhook Jira.
  - `Content-Length` di atas batas dijawab 413 sebelum replika mana pun dihubungi.
  - Body chunked dipotong di batas lalu dijawab 413, dan koneksinya ditutup. Tanpa penutupan, sisa body dibaca
    sebagai request berikutnya (431).
  - Lewat HTTP/2, penutupan berarti GOAWAY. Chromium tetap menerima 413; curl 7.88 kadang hanya melaporkan
    error stream.
- Jawaban error ingress (413, 502) membawa header keamanan yang sama dengan jawaban biasa.
- Tidak ada passive health check (`fail_duration`). Caddy menghitung kegagalan akibat klien (body terpotong, upload
  yang ditolak lebih awal) sebagai kegagalan replika, sehingga replika sehat bisa dikeluarkan dari pool. Dua request
  oversized cukup untuk mengosongkan pool. Replika yang mati dikeluarkan oleh health check aktif (3 s); sampai
  saat itu, kegagalan koneksinya diulang ke replika lain.
- `/metrics` tidak dirutekan ingress.

## Belum diverifikasi di CP8

- Image dan compose belum dijalankan dengan Docker (host CI tidak punya Docker). Skrip produksi diuji native:
  - PG 18.6, age 1.1.1, Caddy 2.11.7, dan binary rilis;
  - drill backup/PITR dan e2e ingress di `evidence/i3/cp8/`;
  - compose diperiksa statis.
- Salinan backup off-site: TUNGGU owner.
- Koneksi app→Postgres di jaringan internal `backend` tanpa TLS. Jaringan ini internal Docker di satu host.
- Tulis non-idempoten yang sedang berjalan di replika yang crash tetap bisa mendapat 502 (bagian 2).
- Body chunked oversized sempat diteruskan sebagian ke replika sebelum dipotong di batas. Replika menolaknya,
  dan replika itu tidak dikeluarkan dari pool.
- Langkah `ALTER SYSTEM RESET` pasca-restore dan perintah `docker run` pada bagian 5 belum dilatih
  dengan Docker.
