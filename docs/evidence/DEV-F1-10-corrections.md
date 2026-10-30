# DEV-F1-10 — Koreksi QA/UX sesudah b8087ce

Owner Developer; reviewer PM/QA/UIUX. Baseline b8087ce, branch pilot/itsm-revamp-v2. Sumber: preparation-v2/05-qa/10-verifikasi-2a68ccb.md dan 04-ux/09-review-2a68ccb.md; ADR-001. Tidak push/deploy atau mengubah legacy.

P0 commit 3bf52a9: BUG-QA-001/002/003 diperbaiki di source dengan regresi HTTP. Seluruh ekspor masked, termasuk SUP/L2. Judul dan catatan daftar laporan dimasking. Tanggal awal/akhir divalidasi secara kalender sebelum konversi WIB. Bukti run 14/14 di developer-correction-p0.tap. Status penutupan independen tetap menunggu QA.

| ID | Implementasi dan evidence | Status acceptance |
|---|---|---|
| UX09-01 | merchant/PIC wajib dengan batas, kategori/tipe dari endpoint skema publik; tersimpan ke tiket dan tersedia saat triase; uji invalid/worker/HTTP | Field inti diperbaiki; lampiran intake publik dan validasi format kontak/kronologi penuh masih terbuka |
| UX09-02 | Penyebab resolusi field tersendiri, teks 1–1000, validasi server dan error terhubung ke field; uji penolakan tanpa perubahan versi | Backend diuji Developer; browser/a11y NOT RUN; enum penyebab regulator tidak diklaim |
| UX09-03 | Preview periode, total lokal/ekspor, ID selisih, row masked/versi; Zammad/Lark null/Tidak tersedia; error tampil tidak lengkap | HTTP/count/role/tanggal diuji; kegagalan request UI dan visual NOT RUN; integrasi nyata belum dikonfigurasi |
| UX09-04 | Draft Map RAM per akun/route/form, pemulihan saat kembali, 401 menjelaskan draft tetap di RAM; unload guard, logout clear; tanpa Web Storage | Implementasi ada, pengujian browser expiry/navigasi masih NOT RUN; tetap terbuka untuk review |
| UX09-05 | Delta b8087ce diuji melalui PATCH triase sesudah DISETUJUI_MANUSIA: kembali DATA_DRAFT v4; stale checker 409; preview/CSV memakai status baru; review ulang mulai REVIEWED | Regresi Developer PASS; QA independen/visual belum PASS |

Validasi final: Node v24.22.1; `npm test` 19/19 PASS; `node --check app/public/app.js` PASS; `git diff --check` PASS. Bukti aktual developer-correction-p1.tap. Uji awal penambahan intake gagal karena batas rate fixture (429); jam fixture kemudian dimajukan antar request; hasil final 19/19. Ini bukti Developer, bukan verifikasi independen.

Run: `npm start`, loopback default 127.0.0.1:3100; fixture sesuai app/README.md, tidak ada akun produksi diperlukan. Tidak menjalankan instance fixture browser baru atau mengubah jaringan. URL fixture aktif terikat SHA masih unresolved. UX09-06..15 di luar koreksi ini tetap terbuka.

Risiko: kontrak intake bertambah wajib; klien lama perlu menyesuaikan. Pending intake lama masih didukung. SQLite upgrade additive dua kolom diuji dengan data lama; backup/rollback pada docs/DEPLOY.md. Rollback kode ke b8087ce atau restore backup sebelum upgrade pada database disposable/persisten yang sesuai; jangan menghapus audit/data langsung.
