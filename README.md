# ITSM API — fondasi Rust (D02a)

Fondasi backend terisolasi: Axum 0.8 + SQLx 0.8 + PostgreSQL. **Hanya UAT dengan data dummy.**
Tidak ada akses production, tidak ada SSO, tidak terhubung ke Zammad.

## Isi
- `api/src/config.rs`: config guard fail-closed. `ITSM_ENV=production` + `uat_superadmin` ditolak; `sso` ditolak sampai auth kantor siap.
- `api/migrations/0001_foundation.sql`: penanda DB uji, company, workspace (1 company = 1 workspace), users (tanpa password), platform grant `uat_superadmin`, membership (`cs`/`lead_cs`), sesi.
- Guard runtime: `serve` menolak start dan `/health/ready` = 503 bila DB tidak ditandai `test_dummy` untuk nama DB yang sama. Entry UAT juga memeriksa ulang penanda di setiap request.
- Sesi server-side: token 32 byte CSPRNG, DB hanya menyimpan SHA-256; cookie HttpOnly, SameSite=Strict, `__Host-` saat Secure; CSRF synchronizer token (`X-CSRF-Token`, constant-time) untuk semua metode unsafe.
- Workspace: akses = membership aktif di workspace aktif, atau grant UAT Superadmin. ID asing/tidak dikenal/malformed = 404.
- Kontrak: `openapi/openapi.json` (OpenAPI 3.1), dicocokkan dengan router oleh `api/tests/contract.rs`.

## Menjalankan
```sh
. scripts/env.sh
export ITSM_ENV=uat ITSM_ACCESS_MODE=uat_superadmin DATABASE_URL=postgres://.../itsm_uat_dummy
cargo run -p itsm-api -- migrate
ITSM_SEED_CONFIRM_DB=itsm_uat_dummy cargo run -p itsm-api -- seed-uat   # menandai DB sebagai dummy
cargo run -p itsm-api -- serve
```
Env: `ITSM_ENV` (uat|production), `ITSM_ACCESS_MODE` (uat_superadmin|sso), `DATABASE_URL`, `ITSM_BIND` (default 127.0.0.1:8080), `ITSM_COOKIE_SECURE` (default true).

## Gerbang QA
`PG_BIN=/path/pg/bin scripts/ci.sh` menjalankan fmt, clippy `-D warnings`, dan test terhadap cluster PostgreSQL sekali pakai. Workflow GitHub di `.github/workflows/ci.yml` (belum pernah dijalankan; repo belum punya remote).

## Belum termasuk
Tiket, SLA, admin grants, audit trail, FE, SSO kantor, deploy.
