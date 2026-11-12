# Paket deploy UAT terbatas (DB dummy saja)

Stack: `ingress` (Caddy 2.11.7: TLS, allowlist IP, basic auth, web statis, proxy `/api` + `/health`)
→ `api` (itsm-api, migrate lalu serve) → `postgres` 18.4 di jaringan privat.
Hanya `ingress` yang mem-publish port. `api` dan `postgres` tidak bisa dijangkau dari luar.

**Lampiran di luar scope (keputusan owner, D13).** UI lampiran tidak ada dan `clamav` **tidak** dijalankan
secara default (compose profile `attachments`). `ITSM_CLAMD_ADDR` kosong, jadi setiap unggahan lewat API
ditolak gagal-tutup (503 `scanner_unavailable`, tidak ada berkas yang ditandai bersih). clamd bukan
dependensi start dan bukan gerbang rilis. Data/skema lampiran historis dan kontrol backend tetap ada.

**Bukan produksi.** `ITSM_ENV=uat` + `uat_superadmin` di-hardcode di `compose.yaml`; API menolak
`serve` bila DB tidak bertanda dummy. Jangan arahkan ke DB berisi data nyata.

**Status verifikasi:** paket ini belum pernah dijalankan dengan Docker (host build tidak punya Docker).
Yang sudah diuji native: guard, Caddyfile di depan API UAT live, entrypoint, dan cek statis compose.
Lihat bukti kandidat. Image belum di-build, container belum dijalankan, compose belum dijalankan.

## Pembatasan akses (wajib, gagal tutup)
Entri Superadmin UAT tidak boleh terbuka ke publik. Ada dua lapis, dan keduanya wajib:
1. **Allowlist IP** (`ITSM_UAT_ALLOWED_CIDRS`) diperiksa pada peer TCP. Peer di luar allowlist
   mendapat 403 sebelum login, dan `X-Forwarded-For` dari klien tidak dipercaya.
2. **Basic auth** dengan hash bcrypt (`caddy hash-password`, cost ≥ 10) dari file secret, di atas TLS.

Stack **tidak bisa start** tanpa keduanya:
- `compose.yaml` memakai `${VAR:?}`, sehingga `docker compose up` gagal bila variabelnya kosong.
- `ingress-guard.sh` (entrypoint ingress) memvalidasi nilainya dan menolak start (exit 64) untuk kasus:
  - `0.0.0.0/0`, `::/0`, atau prefix yang terlalu lebar (< /8 untuk IPv4, < /32 untuk IPv6);
  - keyword seperti `private_ranges`, atau `*`;
  - password polos di file hash;
  - hostname localhost atau hostname dengan skema;
  - TLS yang bukan `internal` dan bukan e-mail ACME.
- Tanpa guard pun, `caddy validate` atas Caddyfile kosong gagal.

Tidak ada konfigurasi contoh yang terbuka. `env.example` sengaja dikirim dengan nilai kosong.

> Peer IP: dengan port publish Docker default (iptables DNAT, IPv4), Caddy melihat IP klien asli.
> Pastikan hal ini di log akses (`remote_ip`) setelah start. Bila ada load balancer/NAT di depan
> host sehingga semua klien tampak satu IP, allowlist itu menjadi lebar. Batasi di LB/firewall juga.

## 1. Persiapan sekali (host operator)
```sh
cd deploy/uat
cp env.example .env                       # isi: ITSM_UAT_IMAGE_TAG=<git SHA>, SITE_ADDRESS, TLS, ALLOWED_CIDRS, BASIC_AUTH_USER
install -d -m 700 secrets
openssl rand -hex 32 > secrets/pg_password                        # alfanumerik, aman di URL
docker run --rm caddy:2.11.7-alpine@sha256:e70b164da81ef698b3ad19af799ea6086ce6d9e48ed4079eb9fe26f6fbf14ee5 \
  caddy hash-password > secrets/ingress_basic_auth_hash           # prompt interaktif; password dibagikan lewat kanal aman
: > secrets/itsm_keyring                                          # kosong = tanpa keyring; isi: lihat deploy/RUNBOOK-keyring.md
chmod 444 secrets/pg_password secrets/ingress_basic_auth_hash secrets/itsm_keyring  # dibaca uid container; direktori 700 membatasi host
./check-config.sh                                                 # wajib OK sebelum up
```
`.env` dan `secrets/` ada di `.gitignore` dan `.dockerignore`. Jangan di-commit, jangan dicetak.

`ITSM_UAT_TLS=internal`: Caddy memakai CA lokal. Distribusikan root CA dari volume `caddy-data`
(`/data/caddy/pki/authorities/local/root.crt`) ke klien UAT. `ITSM_UAT_TLS=<e-mail>`: ACME publik,
yang butuh DNS publik dan port 80/443 terjangkau oleh CA.

## 2. Build (dari root repo, pada SHA kandidat)
```sh
git checkout <SHA>; docker compose -f deploy/uat/compose.yaml build
```
- Base image dipin dengan digest. Digest diambil dari Docker Hub API pada 2026-11-02 dan belum di-pull dari sini.
- Verifikasi dulu: `docker buildx imagetools inspect rust:1.99.1-slim-bookworm` (dan seterusnya), lalu cocokkan dengan `Dockerfile`/`compose.yaml`.
- Build web menjalankan `gen:api` + `check:api` + `tsc -b` + `vite build`. Bundle tidak berisi secret (tidak ada env `VITE_*`).

## 3. Start pertama (DB dummy baru)
```sh
docker compose -f deploy/uat/compose.yaml up -d postgres
docker compose -f deploy/uat/compose.yaml run --rm -e ITSM_SEED_CONFIRM_DB=itsm_uat_dummy api seed-uat   # migrate + tandai dummy + tiket dummy
docker compose -f deploy/uat/compose.yaml up -d
docker compose -f deploy/uat/compose.yaml ps        # postgres, api, ingress healthy (clamav tidak ikut: profile attachments)
```
- Urutan start: `api` menjalankan `migrate` lalu `serve`. Sebelum seed, `serve` menolak start karena DB belum bertanda. Itu disengaja, karena tidak ada auto-seed.
- `ingress` menunggu `api` healthy (`/health/ready`).
- `seed-uat` hanya untuk DB dummy. Entrypoint dan binary sama-sama mewajibkan konfirmasi nama DB.

## 4. Verifikasi setelah start (dari IP di allowlist)
| Cek | Harapan |
|---|---|
| `curl -sI https://<site>/` tanpa kredensial | 401 |
| dengan kredensial | 200, SPA |
| `https://<site>/health/ready` | 200 `ready` |
| dari IP di luar allowlist | 403 (tanpa prompt login) |
| `docker compose ps` | tidak ada service `clamav` (profile `attachments` mati) |
| unggah lampiran lewat API (tidak ada di UI) | 503 `scanner_unavailable` (gagal-tutup) |

## 5. Upgrade ke SHA baru
1. Backup DB dulu (§6), lalu `git checkout <SHA-baru>`.
2. Ubah `ITSM_UAT_IMAGE_TAG` ke SHA baru, lalu jalankan `build` dan `up -d`.
3. `api` menjalankan `migrate` saat start. Semua migrasi bersifat additive (expand).

## 6. Backup / restore
```sh
docker compose -f deploy/uat/compose.yaml exec -T postgres pg_dump -U itsm -d itsm_uat_dummy -Fc > backup-$(date +%F).dump
docker compose -f deploy/uat/compose.yaml exec -T postgres createdb -U itsm <db-dummy-baru>
docker compose -f deploy/uat/compose.yaml exec -T postgres pg_restore -U itsm -d <db-dummy-baru> --no-owner < backup-YYYY-MM-DD.dump
```
- Lampiran historis (bila ada) ada di volume `attachments`. Backup-kan bersama dump.
- Penanda dummy terikat ke nama DB. DB hasil restore dengan nama lain ditolak `serve` sampai
  ditandai ulang lewat `seed-uat`, dan itu hanya boleh untuk DB dummy.
- File dump berisi data UAT dummy. Simpan dengan akses terbatas.

## 7. Rollback
- **Ke image sebelumnya:** set `ITSM_UAT_IMAGE_TAG` ke SHA lama (image harus masih ada atau di-build ulang dari SHA itu), lalu `up -d`.
  - Bila SHA lama punya migrasi lebih sedikit, migrator SQLx di binary lama menolak start karena ada versi yang tidak dikenalnya.
  - Untuk itu, jalankan dulu file `.down.sql` migrasi yang lebih baru, satu per satu dari yang terbaru, di dalam transaksi, setelah backup:
    ```sh
    docker compose -f deploy/uat/compose.yaml exec -T postgres psql -U itsm -d itsm_uat_dummy -v ON_ERROR_STOP=1 -1 \
      -f - < api/migrations/0011_d11_platform_audit.down.sql
    docker compose -f deploy/uat/compose.yaml exec -T postgres psql -U itsm -d itsm_uat_dummy -v ON_ERROR_STOP=1 \
      -c "DELETE FROM _sqlx_migrations WHERE version = 11"
    ```
  - Contoh di atas untuk `0011`. Dari D13 ke D12, jalankan dulu `0012_d13_report_format.down.sql` lalu hapus version 12.
  - `0012` down meng-expire artefak XLSX (binary lama tidak mengenal format) lalu menghapus kolom `format`. Run CSV tetap.
  - `0011` down **menghapus** audit penolakan platform. Backup dulu.
- **Deploy pertama gagal:** `docker compose down` (tanpa `-v`) menghentikan stack dan volume tetap ada.
  - `down -v` menghapus DB dummy, lampiran dan signature. Jangan dijalankan kecuali memang reset total.
- **Akses darurat:** mematikan `ingress` (`docker compose stop ingress`) menutup semua akses luar seketika.

## 8. Batas yang diketahui
- **Lampiran/clamd: di luar scope (D13).** Service `clamav` tetap didefinisikan (image resmi dipin digest,
  healthcheck `clamdcheck.sh`) tetapi hanya di profile `attachments`, mati secara default. Runtime clamd belum pernah diuji.
  - Tanpa `ITSM_CLAMD_ADDR`, unggahan gagal-tutup: 503 `scanner_unavailable`, tidak ada bypass scanner.
  - Mengaktifkan lagi fitur lampiran butuh scope baru: UI, `ITSM_CLAMD_ADDR=tcp://clamav:3310`,
    `--profile attachments`, lalu uji runtime (PONG, EICAR, timeout, `StreamMaxLength` ≥ batas unggah API 10–25 MiB).
- **Integrasi Jira/Lark (CP5b):** default UAT `ITSM_INTEGRATIONS` kosong = `simulation` (tanpa panggilan keluar).
  - Mode `live` butuh keyring di `secrets/itsm_keyring` + `ITSM_KEYRING_ACTIVE` di `.env` (tanpa itu: 503 `keyring_missing`,
    koneksi tidak bisa diaktifkan) **dan** egress HTTPS ke Jira Cloud / Lark. Jaringan `backend` sengaja
    `internal: true`, jadi paket ini **tidak** punya egress: live tidak akan tersambung sampai ada egress
    terkendali (proxy/allowlist domain) yang disetujui security. Isolasi tidak dilonggarkan di paket ini.
  - `ITSM_INTEGRATIONS_LAB=on` hanya untuk stub uji (http/alamat privat); jangan dipakai dengan provider nyata.
  - Keyring dibaca dari file secret (`ITSM_KEYRING_FILE`), bukan env, sehingga tidak terlihat lewat `docker inspect`.
    Rotasi kunci: `deploy/RUNBOOK-keyring.md` (`docker compose run --rm api reseal --dry-run`, lalu `reseal`).
  - Kredensial hanya-tulis (UI: "Tersimpan · diganti <tgl>", ganti/hapus); nilainya tidak pernah keluar dari API, log, atau audit.
- **Rate limit per proses** (satu replika). Jangan scale-out `api` tanpa shared store.
- **Basic auth** adalah gerbang ingress, bukan identitas pengguna. Semua tester tetap masuk sebagai Superadmin UAT, dan itu tidak membuktikan RBAC.
