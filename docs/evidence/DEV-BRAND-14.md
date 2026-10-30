# DEV-BRAND-14 — audit dampak dan dependency visual

Owner Developer; reviewer PM/UIUX/QA. Assignment cf9780fd-52ad-4b01-ad1b-e2c79856be4a. Tanggal 30 Oktober 2026 WIB. Baseline source 32695f60c264a80fede89c5aa1a983780e2d80b5, branch pilot/itsm-revamp-v2. Status: **AUDIT SELESAI, IMPLEMENTASI VISUAL BELUM DIMULAI — dependency spesifikasi**. Bukan klaim task visual selesai atau gate lulus.

## Input dan kondisi aktual

Dibaca langsung: design-reference/deca-qash/README.md; app/public/style.css, index.html; docs/DEPLOY.md dan evidence DEV-F1-13. Working tree awal hanya untracked design-reference/ dan preview-32695f6/. Kedua folder tidak diubah atau di-stage. Tidak ada source write aktif yang terlihat dalam git status awal.

Path handoff yang diminta PM `/data/projects/itsm/preparation-v2/04-ux/12-brand-revamp-spec.md` belum ada saat pemeriksaan; direktori UX terakhir berisi review11. Register brand lokal hanya satu README: tidak ada berkas logo master, font Nunito atau PDF yang hash-nya dapat diverifikasi. Tidak mencoba mengunduh/menafsirkan ulang guideline parsial sebagai spesifikasi final.

Requirement pengguna: acuan DECA/QASH untuk visual F1; business rules, legacy dan deployment automation dipertahankan. Dasar brand yang tersedia: register hal viewer9/cetak06 palet, viewer12/cetak09 larangan modifikasi logo, viewer13/cetak10 Nunito Bold/SemiBold/Regular. Ini fakta dari register UIUX, bukan pembacaan PDF baru oleh Developer. Registrasi belum membuktikan aturan DECA identik QASH. Acuan layar: UX07 §2–4; kontrak teknis PM04 ADR-001. Tidak ada klaim compliance hukum.

## Audit style dan permukaan perubahan

| Area | Aktual baseline | Dampak implementasi setelah handoff |
|---|---|---|
| Token global | --brand #003571, --action #067a28, link/focus #0066d6; hanya Dark Sapphire persis salah satu warna register | Tambah nama token palet resmi dan token UI semantik terpisah; varian aksesibel harus dilabeli keputusan UIUX, bukan warna brand resmi |
| Font | system-ui/-apple-system/Segoe UI; tidak ada @font-face | Nunito hanya self-host bila aset/font/license/provenance tersedia. Selama belum ada, fallback eksplisit tanpa CDN dan tanpa klaim Nunito sudah diterapkan |
| Identitas | Wordmark teks DOB ITSM Platform pada aside/login; tidak ada image logo | Jangan menggambar ulang/mengubah/recolor logo. Logo butuh master+lockup+clearspace yang diverifikasi; identitas teks dapat dipertahankan sampai handoff |
| Shell | sidebar240px desktop,190px tablet; mobile navigasi horizontal; header putih | style.css untuk spacing/nav active/header; app.js hanya class/markup presentasi bila spesifikasi memerlukan; tidak mengganti router/permissions/draft/session |
| Login/intake | .auth max480px, .card, form standar | Treatment visual mengikuti spec, semua label/file/error/receipt dan state yang sudah ada dipertahankan |
| Daftar/triase/detail | .ticket-row, .toolbar, .detail, .message/.internal, .badge | Pastikan status dan jenis pesan tetap teks eksplisit; tidak hanya warna. Breakpoint320/375/768/1024/1440 dan overflow identitas/ID perlu browser |
| KB/dashboard/report/outbox | reuse .card/.grid/.metric/.banner/.error | Terapkan token konsisten tanpa membuat angka/fitur baru; preview INCOMPLETE, fixture label, retry dead dan approval tetap perilaku saat ini |
| Accessibility | min control44px, visible focus, reduced-motion, skip handler baseline13 | Pertahankan kontras/state/keyboard. Token kontras bukan bukti browser atau WCAG lulus |

## Perhitungan kontras terarah (aktual, bukan tes visual)

Node menghitung relative luminance sRGB dengan transfer 0.04045 dan rasio `(Lmax+.05)/(Lmin+.05)`. Ambang internal normal text4.5:1, large text3:1. Tanpa opacity/gradient/image.

| Foreground | Background | Ratio | Implikasi |
|---|---|---|---|
| #FFFFFF | #04CA02 | 2.22:1 | Tidak cocok untuk teks putih tombol, termasuk large text |
| #003571 | #04CA02 | 5.41:1 | Kandidat teks gelap pada hijau, keputusan UIUX |
| #FFFFFF | #0077FF | 4.13:1 | Di bawah normal text4.5; jangan mengganti action/nav teks putih secara buta |
| #003571 | #D5FDC8 | 10.71:1 | Kandidat surface mint + teks sapphire |
| #FFFFFF | #067A28 | 5.49:1 | Baseline action aksesibel, tetapi bukan warna brand resmi menurut register |
| #FFFFFF | #003571 | 12.02:1 | Kandidat action/navigation gelap |

## Acceptance dan verifikasi berikutnya

Dependency minimum untuk implementasi: UIUX handoff12 dengan pemetaan palet ke action/link/focus/selected/error/status, font fallback yang disetujui saat aset belum ada, treatment shell/login/layar F1 serta keputusan identitas teks vs master logo. Aset logo/font hanya blocker bila handoff mewajibkan pemakaian segera; PM/UIUX dapat menetapkan fallback tanpa memalsukan aset.

Setelah handoff: ubah style/markup minimum, pertahankan semua endpoint/business rules dan kontrak sesi/draft; jalankan npm test baseline32 + regresi presentasi yang bermakna bila markup berubah, node --check bila JS berubah, git diff --check dan diff legacy terhadap37a9bba. Review UIUX visual/keyboard/viewport dan QA delta wajib dipisah dari tes DOM double. Tidak menambah tes CSS yang sekadar mencocokkan isi implementasi. Pada audit ini tidak ada kode berubah, sehingga suite produk tidak diulang; baseline32 adalah evidence historis DEV-F1-13, bukan hasil run baru.

## Deployment contract dan fixture

Kontrak tetap docs/DEPLOY.md: Node24.22.1/node:sqlite, dependency npm nol, bind127.0.0.1:3100, APP_DB privat, APP_COMMIT metadata, HTTPS/Secure cookie untuk akses non-loopback. Perubahan presentasi tidak memerlukan migrasi DB atau perubahan routing/backend. Cache-Control no-store dari server tetap; aset baru bila ada harus dapat dilayani allowlist statis eksplisit tanpa membuka traversal. Jangan menggunakan CDN/font eksternal atau mengubah CSP hanya demi visual.

Tidak ada fixture baru yang dijalankan pada audit ini. preview-32695f6/ tidak diperiksa/diubah dan tidak dianggap layanan aplikasi autentik penuh. Browser tab belum diuji pada task ini; URL aktif terikat SHA, provenance archive/runtime, login fixture aman dan native browser acceptance tetap belum tersedia/terverifikasi. Jangan membuka jaringan publik atau mengubah SCO untuk mengatasinya. Akun fixture hanya disposable lokal dan password input aman sesuai REVIEW.md, tidak chat.

## Risiko, rollback dan status F1

Risiko implementasi: palet terang menurunkan kontras; pergantian font mengubah wrapping; logo tanpa master melanggar guideline; perubahan DOM dapat merusak handler/draft/accessibility. Ini daftar dampak konkret untuk spec, bukan alasan mengubah kontrol bisnis.

Audit ini hanya dokumen; rollback tidak menyentuh database/proses. Untuk delta visual mendatang: stop bila diperlukan, simpan backup sesuai DEPLOY.md, kembali ke archive32695f6 dan restart dengan DB salinan yang sama kompatibel; tidak melakukan downgrade schema, mengembalikan approval atau deploy legacy secara otomatis. Push dapat auto-deploy, sehingga tetap dilarang sebelum integrasi review UIUX+QA delta+PM kompatibilitas/rollback. Tidak push, merge, deploy atau mengubah automation/live.

Gap F1 tetap menurut STATUS_F1.md: SLA pinning/reopen/alerts/auto-escalation, config maker-checker, merge/pagination, dashboard/metrik/manifest/CIF, seluruh matriks akses dan runtime UX yang belum diuji. Revamp visual tidak menutupnya. PM perlu melanjutkan handoff12; task implementasi tetap pending, bukan selesai karena audit sudah dibuat.

## IMPLEMENTASI — menggantikan status dependency audit di atas

Handoff UX12 tersedia dan dibaca penuh pada assignment lanjutan3007c34a. BR12-01..10 diterapkan pada layer presentasi dengan batas review di bawah. Baseline delta639aea3 (source32695f6); branch sama. Tidak ada perubahan business rules/backend/API/schema/permissions/lifecycle. Status implementasi **siap review**, bukan visual PASS atau izin push.

### Changed files dan provenance

- app/public/style.css: token lima warna resmi, primary green/sapphire + hover Eco, border sapphire, nav mint/sapphire aktif dengan weight+indikator, focus3px, font16/metadata14/heading28/24, radius8/12, shell240/190/mobile, header wrap, cards, KPI4/2/1, typography/wrapping/state/component styling.
- app/public/app.js: data-field pada label untuk grid intake, kelas intake/receipt/knowledge, komposer data-visibility+label status eksplisit mengikuti pilihan dan draft yang dipulihkan. Event select hanya mengubah presentasi. Handler submit/logout/epoch/KB/router tetap.
- app/test/ui.test.js: assertion komposer internal terlabel dan tetap internal sesudah draft direkonstruksi; suite terarah lama tetap dipakai.
- Evidence TAP dan dokumen ini. Tidak ada font/image/logo baru, CDN atau CSP diubah. Nunito,system-ui,sans-serif hanya preferensi font; fallback aktual tidak diklaim Nunito. Logo NOT AVAILABLE; nama produk teks biasa. Brand PARTIAL menurut BR12-02/03, diizinkan handoff PM.

### Matriks route/state dan acceptance

| Route/komponen | Implementasi presentasi | Verifikasi / batas |
|---|---|---|
| #login | kartu max480 konten, primary penuh, input/alert/fokus jelas | regresi logout/expiry DOM suite; browser NOT RUN |
| #intake | konten max720, merchant/PIC/type/category grid2→1, native file, receipt mint/token wrap | lost-response/schema retry/draft/file HTTP/DOM lulus; receipt polling tetap notice lama (UX09-07 open) |
| #tickets/#mine/#triage | toolbar wrap, link judul, row fleksibel→stack, status/SLA teks | query/filter/nav DOM lulus subset; pagination/label rinci lama tetap gap |
| #ticket/id | detail2:1→1, kartu/meta/riwayat/lampiran, komposer publik vs internal | macro append+RAM/visibility restored/regresi lifecycle lulus; permission backend tetap |
| #knowledge | search area max720, article75ch, section/badge/error/empty | KB retry/query/filter/stale/empty DOM lulus |
| #dashboard | metric4→2→1, green accent/sapphire angka, status list | hitungan business rules tidak berubah; browser/overflow NOT RUN |
| #reports | kartu record/checker/export/preview, warning/error menetap dan period/stale tetap | preview late-response/export/checker HTTP/DOM lulus; nested section existing tetap, sumber eksternal incomplete |
| #outbox | event wrap, status badge/attempt/retry berizin | auth/retry fault HTTP lulus; tidak ada indikator live palsu |
| unknown route | heading+nav existing, token global | source retained; browser NOT RUN |
| semua state | error merah, empty muted, banner mint, disabled readable, focus/reduced-motion | loading/receipt/draft/expiry teks dipertahankan; seluruh state×role tidak dinyatakan teruji |

BR12-01: implemented; kontras final sapphire/green5.41, sapphire/Eco7.50, sapphire/mint10.71, white/sapphire12.02, error/bg6.80 dan warning/bg6.37 (sRGB; lihat hasil hitung final di bawah bila pembulatan berbeda). Link sapphire/white12.02. Selected focus memakai inset sapphire di mint; rail focus putih. Biru hanya border/aksen, bukan normal small text. Disabled #334155/#e2e8f0 dipertahankan. Kontras token tidak membuktikan WCAG/browser.
BR12-02/03: PARTIAL/fallback tanpa aset rekaan. BR12-04/05/07/08/09: implemented pada route/component yang ada, regressions subset. BR12-06: **NOT RUN**, screenshot320/375/768/1024/1440, keyboard200%, native focus/scroll/zoom memerlukan browser nyata. BR12-10: gap lama tetap terbuka.

### Perintah, output dan reviewer

Node24.22.1. `npm test > docs/evidence/dev-brand-14-suite.tap 2>&1`: exit0,32 PASS/0 FAIL/0 SKIP. Setelah assertion tambahan komposer, `node --test app/test/ui.test.js > docs/evidence/dev-brand-14-ui.tap 2>&1`: exit0,9 PASS/0 FAIL. Tests ini mengeksekusi HTTP/SQLite dan renderer DOM double; bukan screenshot atau layout engine. `node --check app/public/app.js`, `git diff --check` lulus. Tidak perlu tes yang sekadar mengunci string CSS. SHA256 source/output tercatat pada bagian berikut. QA delta dan UIUX visual independen SHA commit baru belum dilakukan.

### Deployment, fixture dan rollback delta

Deployment contract audit di atas tetap. Aset perubahan hanya CSS/ES module yang sudah dilayani server Node; tidak ada endpoint/aset baru, dependency atau migrasi. Legacy index.html/api/deploy unchanged terhadap37a9bba. Push tetap ditahan sampai UIUX+QA+PM merekonsiliasi gate; tidak perlu approval owner ulang setelah gate, tetapi gate belum lulus pada handoff ini.

Tidak menyalakan fixture persisten atau memodifikasi preview-32695f6. Tidak ada URL aktif baru yang bisa dibuktikan terikat SHA ini; browser screenshots belum ada. Blocker konkret review: Browser tab perlu pemetaan yang didukung ke layanan loopback disposable pada environment source, beserta provisioning fixture aman menurut REVIEW.md. Percobaan awal task terdahulu loopback ditolak surface; tidak membuktikan semua browser tidak tersedia. Tidak mengubah jaringan/membuka port publik untuk mengatasinya. Review bisa mulai archive SHA immutable, sementara PM menyelesaikan akses Browser tab yang didukung; preview statis lama bukan bukti login/role/API produk baru.

Rollback visual: stop proses bila mengganti checkout, backup sesuai DEPLOY.md, gunakan archive639aea3/source32695f6 dan restart. Schema tetap; tidak restore approval/DB untuk sekadar mengembalikan CSS. Ini mengembalikan style lama dan menghilangkan label presentasi komposer; tidak menghapus koreksi business rules baseline. Gap F1 pada STATUS_F1.md tetap berlaku; styling bukan penutupan SLA/config/merge/pagination/dashboard/manifest/CIF atau seluruh UX09.

### Hash output final

- f08120fdc1ada894993c2b8cc17436b1acba4ebdda18b31b48281055267d14b8 app/public/style.css
- 5e9497f002b4634babe0f1a5f2d57cb53acaeb71f6eaa9b95b934a7fbc4fd1f5 app/public/app.js
- 3f55475f9d560f690cb4b270a4b23177651dcb8a860de09cf768b24082a23f21 docs/evidence/dev-brand-14-ui.tap
- 3e0263f4d42cf245d3907e9ca898df2f373a67f2e864acf4f3db5415fd83c048 docs/evidence/dev-brand-14-suite.tap

Kontras final terhitung (mengoreksi estimasi naratif bila berbeda):
- #003571/#04CA02: 5.41:1
- #003571/#58EA60: 7.64:1
- #003571/#D5FDC8: 10.71:1
- #991B1B/#FEE2E2: 6.80:1
- #92400E/#FEF3C7: 6.37:1
- #334155/#e2e8f0: 8.40:1
