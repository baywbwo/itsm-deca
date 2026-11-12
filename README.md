# ITSM API — fondasi Rust (D02a + D02 + D03)

Fondasi backend terisolasi: Axum 0.8 + SQLx 0.8 + PostgreSQL. **Hanya UAT dengan data dummy.**
Tidak ada akses production, tidak ada SSO, tidak terhubung ke Zammad.

## Isi
- `api/src/config.rs`: config guard fail-closed. `ITSM_ENV=production` + `uat_superadmin` ditolak; `sso` ditolak sampai auth kantor siap.
- `api/migrations/0001_foundation.sql`: penanda DB uji, company, workspace (1 company = 1 workspace), users (tanpa password), platform grant `uat_superadmin`, membership (`cs`/`lead_cs`), sesi.
- Guard runtime: `serve` menolak start dan `/health/ready` = 503 bila DB tidak ditandai `test_dummy` untuk nama DB yang sama. Entry UAT juga memeriksa ulang penanda di setiap request.
- Sesi server-side: token 32 byte CSPRNG, DB hanya menyimpan SHA-256; cookie HttpOnly, SameSite=Strict, `__Host-` saat Secure; CSRF synchronizer token (`X-CSRF-Token`, constant-time) untuk semua metode unsafe.
- Workspace: akses = membership aktif di workspace aktif, atau grant UAT Superadmin. ID asing/tidak dikenal/malformed = 404.
- `api/migrations/0002_d02_domain.{up,down}.sql` (D02): katalog per workspace, tiket dengan nomor per workspace (`KODE-n`, kode workspace dibekukan setelah nomor pertama), guard lifecycle (resolve wajib ringkasan + tanpa related work blocking terbuka; reopen ke `open` cycle berikutnya dengan alasan; `closed` hanya arsip baca), related work, relasi Jira/Lark (hanya `simulation`), outbox per provider, attachment (bind hanya saat `clean`), report run operasional, audit append-only. Semua relasi anak memakai FK komposit `(workspace_id, id)`; `workspace_id` tidak bisa diubah. Rollback: `MIGRATOR.undo` ke versi 1 (lihat tes `migration_0002_rolls_back_and_reapplies_on_dummy_data`).
- Baca terbatas D02: status tiket, katalog workspace, detail tiket, metadata attachment, metadata report run. Mutasi, list dan unduhan menyusul D03+.
- `api/migrations/0003_d03_grants.{up,down}.sql` + `api/src/authz.rs` (D03): fondasi izin server-side, default deny. Izin bisnis statis per peran di kode (`cs`; `lead_cs` = `cs` + `ticket.assign_any`). Hak admin/integrasi (`admin.members`, `admin.settings`, `integration.manage`, `integration.credentials`, `integration.recovery`) hanya dari baris `workspace_grants` eksplisit per membership; Lead CS tidak otomatis memilikinya. Grant hanya bisa dicabut sekali, tidak bisa diubah/dihapus (IT422), FK komposit ke membership di workspace yang sama; identitas membership (`workspace_id`, `user_id`) dikunci. Workspace tak terlihat = 404, workspace terlihat tanpa izin = 403 `forbidden`. Guard domain IT422 = 422 `rule_violation` dengan pesan generik. Rollback: `MIGRATOR.undo` ke versi 2 (tes `migration_0003_rolls_back_and_reapplies_on_dummy_data`).
- Metadata untuk FE (bukan sumber akses): `GET /api/v1/permissions` (katalog izin + default peran), `GET /api/v1/workspaces/{id}/permissions` (izin efektif pemanggil), `GET /api/v1/workspaces/{id}/grants` (butuh `admin.members`). `GET /api/v1/meta` memuat `uat` (banner persisten, data dummy, `superadminProvesRbac=false`) hanya di mode UAT.
- Kontrak: `openapi/openapi.json` (OpenAPI 3.1), dicocokkan dengan router oleh `api/tests/contract.rs`.

## Menjalankan
```sh
. scripts/env.sh
export ITSM_ENV=uat ITSM_ACCESS_MODE=uat_superadmin DATABASE_URL=postgres://.../itsm_uat_dummy
cargo run -p itsm-api -- migrate
ITSM_SEED_CONFIRM_DB=itsm_uat_dummy cargo run -p itsm-api -- seed-uat   # menandai DB sebagai dummy
cargo run -p itsm-api -- serve
```
Env: `ITSM_ENV` (uat|production), `ITSM_ACCESS_MODE` (uat_superadmin|sso), `DATABASE_URL`, `ITSM_BIND` (default 127.0.0.1:8080), `ITSM_COOKIE_SECURE` (default true). MFA (mode password): `ITSM_MFA` (on|off, default off = login sandi biasa tanpa MFA/step-up); bila `on`, wajib di production: `ITSM_KEYRING`, `ITSM_KEYRING_ACTIVE`, `ITSM_MFA_RECOVERY_PEPPER` (lihat `deploy/uat/env.example`, hanya nama).

Integrasi Jira/Lark: `ITSM_INTEGRATIONS` (live|simulation; kosong = simulation di UAT, live di production; production menolak simulation; nilai lain = gagal start), `ITSM_INTEGRATIONS_LAB` (on|off, default off; on mengizinkan `http://` dan alamat privat/loopback untuk stub uji, ditolak di production), `ITSM_INTEGRATIONS_TIMEOUT_MS` (500–8000, default 5000). Mode live butuh `ITSM_KEYRING` + `ITSM_KEYRING_ACTIVE` (keyring yang sama dengan MFA, dimuat walau MFA off): tanpa itu kredensial tidak bisa disimpan (503 `keyring_missing`) dan koneksi tidak bisa diaktifkan. Kredensial (API token Jira, app secret Lark) hanya-tulis: dienkripsi AES-256-GCM, tidak pernah dikirim balik, tidak masuk log/audit. WhatsApp, Email, dan Website belum tersedia.

## Gerbang QA
`PG_BIN=/path/pg/bin scripts/ci.sh` menjalankan fmt, clippy `-D warnings`, dan test terhadap cluster PostgreSQL sekali pakai. Workflow GitHub di `.github/workflows/ci.yml` (belum pernah dijalankan; repo belum punya remote).

## Belum termasuk
Endpoint mutasi tiket/intake, SLA, mutasi grant/membership (D09), penulisan audit, worker outbox, scan clamd, FE, SSO kantor, deploy.

## Deploy UAT terbatas
`deploy/uat/` berisi compose (ingress Caddy + api + postgres + clamd), Dockerfile, guard akses ingress
yang gagal tutup, dan runbook (`deploy/uat/README.md`). Hanya untuk DB dummy UAT.
