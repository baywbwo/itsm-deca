# DEV-F1-13 — Lifecycle draft dan koreksi UX terarah

Owner Developer; reviewer PM/QA/UI/UX. 30 Oktober 2026 WIB. Requirement REQ-001/008/030/032, AC-F1-002/030/051/061; UX11-01, UX09-12/13. Baseline immutable dcf055f796c2271f2ce64c4555bd894a3c631efa, branch pilot/itsm-revamp-v2. Input: preparation-v2/04-ux/11-review-dcf055f.md, runner UX11-01 dan output reviewer. Tidak membuat assignment paralel atau mengubah salinan reviewer; source baseline awal bersih.

## Hasil dan mekanisme

UX11-01 implemented + tested Developer. Epoch berubah pada logout sukses dan pergantian identitas autentikasi; form/input/submit lama tidak dapat mengisi draft RAM kembali. Respons request lama ditolak sebelum digunakan, termasuk setelah parsing JSON. Intake menahan intent creation setelah pembacaan berkas dan receipt/reset setelah respons jika epoch sudah berubah. Unggah berkas operator juga memeriksa epoch setelah FileReader. Perubahan epoch tidak membatalkan transaksi yang sudah diterima backend; ini perlindungan state UI, bukan rollback server.

Logout gagal tidak mengganti epoch atau menghapus draft. Expiry mempertahankan identitas terakhir dan epoch: login ulang akun semula memulihkan draft dan key intent; login akun berbeda menghapus RAM serta menonaktifkan callback lama. Tidak menggunakan Web Storage. Logout sukses menghapus drafts/intents; detached input/submit juga diabaikan. RAM tetap hilang pada reload.

UX09-13 koreksi handler skip-link mencegah default fragment #main, memfokuskan main dengan tabindex=-1 tanpa mengubah hash router. UX09-12 koreksi parsial: KB memakai query draft saat initial load; kegagalan mengganti loading dengan alert + retry menggunakan query sama; generation guard mengabaikan hasil pencarian basi. Aspek lain UX09-12 (asosiasi error heuristik/validasi/reader/fokus nyata) tetap terbuka.

## Reproduksi, perintah dan output aktual

Node v24.22.1. Sebelum perubahan source, runner reviewer disalin ke app/test/ux11-late-submit.test.js:

- `node --test app/test/ux11-late-submit.test.js > docs/evidence/dev-f1-13-baseline.tap 2>&1`: exit **1**, 0 PASS/1 FAIL, invariant dirty=false gagal (actual true).
- Sesudah koreksi, `npm test > docs/evidence/dev-f1-13-suite.tap 2>&1`: exit **0**, **32 PASS/0 FAIL**. Suite mencakup seluruh 26 tes sebelumnya dan 6 tambahan. Runner reviewer hanya diperbarui exposure harness agar setUser memakai lifecycle produksi; assertion failure asli tidak dikurangi.
- `node --check app/public/app.js` dan `git diff --check`: exit **0**.

Regresi tambahan: delayed intake success/rejection × logout/account switch, draft/intent akun baru tetap utuh, stale form input/submit tidak menulis RAM; 401/expiry → akun semula query/draft/key tetap sama → receipt menghapus intent; akun lain menghapus draft; pending submit gagal setelah logout audit fault masih dapat recovery. Regresi D10-01 tetap memakai HTTP/SQLite aktual: receipt hilang → navigasi/schema failure/retry → key sama dan satu intake. Backend auth/retry/atomicity suite tetap lulus. KB initial error/retry/query/empty serta skip preventDefault/focus/route diperiksa terarah.

Output dan hash SHA256:

```
0f32111bacb8597d6dcf243b384c4c2c80a88b5b73639a40260caa7c660a55aa  dev-f1-13-baseline.tap
9a8f572ba5e9b11ff023289e0d4a834860efcd6c5d004a52e290bebdcb0be30e  dev-f1-13-suite.tap
83dd44ed9ebd47464a9112555f0aa7628080a1075c28bcaa1b2227326892412b  app/test/ux11-late-submit.test.js
62e0e3260c095eb68c6469970b47915c9bf8468d8bdf67a3258e5c5a6704713d  app/test/ui.test.js
```

## Batas bukti, risiko dan rollback

Tes UI menggunakan node:vm + DOM double, bukan browser. QA independen delta ini belum dijalankan. Browser/visual/a11y, native FileReader/pilih ulang lampiran, focus/keyboard engine, confirm/beforeunload dan expiry browser **NOT RUN**. Tidak ada URL fixture aman aktif terikat SHA yang diverifikasi pada task ini. Untuk review Browser tab: instance disposable reachable dengan provenance archive SHA, DB sintetis, fixture OPR/SUP/CMR/ADM melalui provisioning aman (tanpa password chat), kontrol error/expiry/logout fault dan berkas sintetis identik untuk replay. Tidak membuka publikasi atau jaringan baru.

UX09-01..11/14/15 mempertahankan status open/partial dari UX11; D10-02/UX09-04 hanya dikoreksi pada cakupan race dan DOM di atas. F1 SLA/config/merge/pagination/dashboard/manifest/CIF dan matriks security penuh tetap gap. Tidak dipindahkan ke F2 sepihak; bukan persetujuan rilis.

Tidak ada perubahan schema/backend/legacy/deploy/network, push atau live writes. Risiko: request yang sudah terkirim dapat selesai di server sesudah logout; UI mengabaikan hasil sesi lama. Callback in-flight lintas perubahan akun dibuang sengaja. Rollback lokal: hentikan proses, ikuti backup pada docs/DEPLOY.md, revert commit DEV-F1-13 atau jalankan archive baseline dcf055f dengan DB salinan; tidak ada migrasi. Rollback mengembalikan race UX11-01 dan bug KB/skip-link. Target akhir QA memakai SHA commit yang diberikan pada handoff; bukan APP_COMMIT sebagai bukti tunggal.
