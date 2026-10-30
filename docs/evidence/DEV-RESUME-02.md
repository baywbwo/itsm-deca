# DEV-RESUME-02 — UX13 B13-01..03

Owner Developer; reviewer UIUX/QA, integrasi PM. Baseline a9fe56b, branch pilot/itsm-revamp-v2. Input UX13 bagian B13-01..03; QA16 hasil fungsi delta PASSED pada subset dengan browser NOT RUN. Scope BR12-01/05/06/09; AC-F1-061. Ini koreksi presentasi, bukan closure seluruh F1.

- B13-01: border komposer normal/internal sekarang sapphire. Border kiri internal tetap warning, biru normal dipertahankan sebagai aksen master. Focus global sapphire3px/offset3 tetap; disabled button memakai border lama di surface putih dan teks terbaca. Komposer tidak punya disabled mode sendiri; saat submit hanya button disabled, perimeter tetap sapphire. sRGB sapphire/F6F8FB=11.29, sapphire/FEF3C7=10.68; warning/FEF3C7=6.37; sisi luar putih sapphire=12.02. Angka final hasil script pada handoff mengoreksi pembulatan ini. Browser computed/focus clipping tetap NOT RUN.
- B13-02: .card p/small/label/li memakai overflow-wrap:anywhere dan min-width0; mencakup metadata kontak/intake/merchant/PIC pada semua proyeksi role. Tidak menambah overflow:hidden, truncate atau mengubah isi/proyeksi backend.
- B13-03: label visible Status kerja dan SLA resolusi di daftar tiket pada semua viewport; paused tetap dijeda, nilai menit/hitungan sama. Bukan CSS generated content yang dapat hilang pada pembaca layar.

Changed files: app/public/style.css, app/public/app.js, app/test/ui.test.js; evidence ini dan TAP. Tidak ada backend/schema/permission/session/asset master atau legacy berubah. Font/logo fallback PARTIAL tetap.

Verifikasi: `node --test app/test/ui.test.js app/test/ux11-late-submit.test.js > docs/evidence/dev-resume-02-ui.tap 2>&1`:10 PASS,0 FAIL/skip, exit0. Fixture daftar ditambah status/SLA42 dan token judul200 karakter, assertion label+nilaidata utuh. Suite juga mencakup draft/macro/KB/filter/preview/logout/epoch/late-submit lama. `node --check app/public/app.js` dan git diff --check PASS; legacy diff terhadap37a9bba kosong. Tidak mengulang272 assertion backend QA16 karena backend tidak berubah; QA delta reviewer tetap diperlukan. Tes DOM double tidak mengukur wrap/layout/browser, zoom atau kontras computed.

Fixture akan dibuat dari git archive SHA koreksi setelah commit; path dan hash archive/proses/database dicatat pada handoff/provenance di app/data (ignored), tanpa memindahkan preview-32695f6 atau design-reference. Bind loopback3102, disposable SQLite baru, APP_FIXTURES=1/NODE_ENV=development, tidak public. Akun reviewer dapat provision menurut docs/REVIEW.md dengan password input lokal tersembunyi ke database baru; password tidak chat. Proses fixture awal hanya demonstrasi public intake, akun login tidak disediakan tanpa provisioning aman. Jangan menggunakan akun production. Pemilihan kontak/merchant/PIC panjang, restricted roles, native FileReader/viewport200% memerlukan uji browser sesungguhnya.

Kontrak deploy docs/DEPLOY.md tidak berubah: Node24.22.1, dependency0, private DB, TLS/secure cookie bila non-loopback. Push bisa auto-deploy; belum push sebelum review UIUX+QA+PM. Rollback presentasi: stop, backup konsisten, gunakan archive a9fe56b dan DB salinan sama (schema tidak berubah). Jangan restore data/approval untuk rollback CSS. Risiko: prefix label menambah wrapping, perlu viewport320/375/768/1024/1440; wrapping lossless belum dibuktikan engine browser. Gap SLA/config/merge/pagination/dashboard/manifest/CIF, master font/logo dan native UX masih terbuka.

Hash SHA256:
- 7b2fc3fba67b84e71557ce667c2639b0a8b38a79c071847796e63850227897c0 app/public/style.css
- 4e4fd1c09fe0380faa1cada5ed2e0bc82e062f738fd30a6d5688904004fe191b app/public/app.js
- 11c658d38a24a0e21f80f38e342dab8bbf913c5fc93c4affc9e23109eac0fe18 docs/evidence/dev-resume-02-ui.tap

Rasio final flat color (menggantikan estimasi di atas):
- #003571/#f6f8fb: 11.294:1
- #003571/#fef3c7: 10.791:1
- #92400e/#fef3c7: 6.367:1
- #8a94a6/#ffffff: 3.058:1
- #334155/#e2e8f0: 8.399:1
