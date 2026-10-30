# DEV-RUN-17 — proses lokal nyata dan recovery

Requirement: DEV-RUN-17 owner PM, AC koreksi B13-01/B13-02 dan BR12/AC-F1-061 dari DEV-RESUME-02. Sumber kontrak docs/DEPLOY.md; praktik SDLC S08 melalui preparation v2. Tanggal 2026-10-30. Developer self-verification, bukan hasil QA/UX independen.

## Identitas dan isolasi

SHA aplikasi immutable a727f14c086d8e850add595d8758488d77a19e4f, branch pilot/itsm-revamp-v2. Source diekstrak dengan git archive ke /data/projects/itsm/revamp-v2/app/data/dev-run-17. Tidak memakai DB live. Node v24.22.1, dependency npm nol, NODE_ENV=development, APP_FIXTURES=1, max-old-space-size=128 MB, HOST=127.0.0.1 PORT=3103. RSS terukur 78096 KiB. DB sintetis baru; password random hanya memori/environment proses fixture singkat, tidak dicetak/disimpan. Tidak ada public listener/proxy/tunnel dibuat. Untracked design-reference/ dan preview-32695f6/ tetap utuh.

## Hasil aktual

PASS npm test 32/32, fail/skip 0; output dev-run-17-suite.tap. PASS runner HTTP dev-run-17-runner.mjs (exit 0), output dev-run-17-http.log:

- Proses terpisah menjalankan app/server.js dari archive; /healthz 200 dan /version memuat SHA penuh.
- Login OPR 200, cookie HttpOnly/SameSite; 3 tiket fixture tersedia.
- POST internal dan reply 201; GET membuktikan kedua kind tersimpan berbeda.
- Upload lampiran 201, download 200 dan byte identik.
- Intake publik 202; polling membuktikan worker proses nyata membentuk tiket keempat.
- Backup saat server aktif melalui app/backup.js; integrity_check OK.
- SIGTERM exit0; restore backup ke file baru; startup kembali health200, sesi/tiket/pesan/lampiran tetap utuh.
- Logout200 lalu /api/me401; shutdown restore exit0.

Runner disalin ke evidence untuk audit. Untuk reproduksi: archive SHA ke direktori disposable BARU, salin runner ke root archive, jalankan node run.mjs (port3103 harus kosong). Runner sengaja menolak benturan DB backup melalui perilaku backup existing-file; jangan gunakan DB operasional. Setelah selesai runner meninggalkan fixture review tanpa menyimpan password.

## Handoff fixture dan batas browser

Fixture masih running PID109970, URL http://127.0.0.1:3103; runtime.json pada app/data/dev-run-17 mencatat PID/SHA/path. DB aktif app/data/dev-run-17/restored.sqlite. Curl /version setelah runner selesai masih200 SHA sesuai. Reviewer dapat memakai HTTP lokal dari lingkungan kode yang sama. Password ephemeral sudah dibuang; provisioning review auth memerlukan proses lokal aman docs/REVIEW.md, tanpa kredensial chat.

Percobaan Browser tab bawaan ke URL ini gagal net::ERR_SOCKS_CONNECTION_FAILED; hasil browser menyatakan layanan loopback diblokir karena dianggap bukan layanan yang dimulai bot. Tidak mencoba jalur browser lain atau membuka jaringan publik. Visual/keyboard/responsive nyata NOT RUN. Pembatasan ini spesifik akses browser ke fixture, bukan bukti aplikasi gagal start. Handoff QA/UX diperlukan melalui PM; jangan klaim review UX selesai.

## Koreksi, kompatibilitas dan rollback

Diff a9fe56b..a727f14 mengonfirmasi koreksi border sapphire dan overflow-wrap:anywhere/min-width:0; tidak ada perubahan backend/schema. Tidak ditemukan bug konkret pada smoke ini; tidak menambah perubahan source. Pengukuran wrap/computed contrast masih memerlukan browser.

Kontrak Node24.22.1 dan schema tetap; tidak menjalankan deploy/deploy.sh atau mengubah automation. Push berpotensi auto-deploy: belum push/merge/deploy. Kompatibilitas routing/runtime tujuan eksternal belum diverifikasi; acceptance release PM dan QA/UX terbaru tetap pending. Batas F1 dan integrasi mock di DEPLOY.md tetap berlaku.

Shutdown fixture: kill -TERM 109970 setelah verifikasi PID/args dan runtime.json. Recovery sudah diuji: stop, copy snapshot ke path baru, APP_DB menunjuk salinan, jalankan archive SHA dengan environment fixture; jangan menimpa DB terbuka. Rollback presentasi ke a9fe56b memakai salinan DB konsisten yang sama, tanpa restore data lama; tidak ada migrasi delta UX. Jangan mengembalikan approval/data operasional hanya demi CSS.

Evidence SHA256:
- runner aaad724da1bb2da1187a481143a61e923f127fb9a8522c0b17382eccbb7a63cf
- HTTP 1f1753fd8871003f200caf873beba81ed2a5d113891e47c1a973be6f8d3e3b83
- suite 83a636736e4e797d107669efbebc94360b6009d5adf5f658890bd5c8e4107940
