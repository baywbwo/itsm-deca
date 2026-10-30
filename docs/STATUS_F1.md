# DEV-F1-09 — Status implementasi aktual F1

Owner Developer; reviewer PM + QA + UX/Compliance; tanggal 30 Oktober 2026 WIB. Input: PM 04 ADR-001/Fase, BA 08 rev2 §4/7, UX 07 §3/4 dan 08 kebutuhan kritis, Compliance 06 §6.2. Baseline 2a68ccb pada worktree /data/projects/itsm/revamp-v2 branch pilot/itsm-revamp-v2. Source dibaca langsung: app/domain.js, server.js, public/app.js, fixture.js, tests, docs. Tidak mengulang discovery live. Status: F1 parsial, bukan siap produksi. SHA delta tersedia pada handoff commit DEV-F1-09; artifact ini tidak menjadi klaim QA independen.

## Implemented / tested / gaps

“Tested” di bawah = uji developer pada produk HTTP/SQLite, bukan lulus seluruh AC. Bukti 12/12 npm test pada Node 24.22.1: docs/evidence/developer-product-tests.tap. QA menguji SHA baseline terisolasi; hasilnya belum diterima/direkonsiliasi di sini.

| AC-F1 | Implemented dan tested developer | Gap / belum dibuktikan |
|---|---|---|
| 001–007 | Intake async 202, token 192 bit, replay 200, konflik payload, polling, rate-limit IP/kontak; tes 10 paralel | AC 50 paralel belum terpenuhi oleh tes (rate limit 10/IP); kontrak replay/rate-limit perlu diselaraskan PM. Token expiry 7 hari; kanal non-email label manual, pencatatan kanal respons terstruktur belum ada |
| 008–010 | Triase type/category/severity/complaint, audit, textContent UI dan redaksi regex 16 digit | SLA triase/alert belum ada; regex tidak membuktikan semua format identitas berspasi/unicode terblokir |
| 011–016 | Matriks transisi dasar, version CAS, claim conflict, complaint enum config/taxonomy.json, pesan/internal, auto-close | Belum tes seluruh sel transisi/peran; merge ditolak eksplisit; riwayat API hanya metadata tanpa before/after. Delta: auto-close atomik+outbox sekali, dibuktikan fault injection audit |
| 020–025 | Menit kerja WIB/weekend/libur, pause resolution, first-response, alert resolution 75/100 tersimpan | Belum oracle independen 200 kasus/due date; reopen menghitung periode Teratasi, pinning kalender/versi belum ada; response/triage alerts, auto-escalation/recipient queue, manual escalation level belum lengkap. Alerts belum punya halaman in-app khusus. Konfigurasi maker-checker tidak tersedia, edit API fail-closed |
| 030–034 | Scrypt, cookie HttpOnly/SameSite, expiry/idle, logout, Origin/Sec-Fetch guard, deny-by-default, audit CMR/AUD, login limiter; tes HTTP dan matriks allow() | Belum matriks seluruh endpoint×role/IDOR, login lockout AC spesifik, audit denial correlation ID; matriks allow() bukan bukti keseluruhan API. Mutasi login/logout/attachment/outbox retry belum atomik dengan audit; prioritas keamanan berikutnya |
| 040–042 | Promosi eksplisit, maker/checker DB CHECK, CMR sequential status + catatan, optimistic version | Delta: update sumber membatalkan reviewed/approved → DATA_DRAFT, checker/note NULL, version+1 + audit satu transaksi; approval stale 409 teruji. Perubahan flag setelah review tetap ditolak. Belum hash/snapshot sumber immutable |
| 043–046 | Periode WIB half-open, CSV prefix neutralisasi NFKC, alasan/tujuan, export ID/count/checksum, identity tidak tersedia, exclude is_test | Belum uji batas tengah malam lengkap; metadata generated_by/at/filter tidak disimpan sebagai manifest terstruktur; endpoint koreksi is_test belum ada. Ekspor khusus fixture opt-in belum ada; default seed export kosong sengaja |
| 050 | Allowlist/ext/magic bytes/ukuran/jumlah, raw attachment download nosniff, role masked ditolak, traversal dibatasi regex | Validasi konten tidak setara pemindaian malware; pemeriksaan identitas dalam berkas biner tidak membuktikan minimisasi; belum semua boundary/permission tests; mutation+audit atomicity belum ada |
| 051 | KB query, macro mengisi draft | Belum seed 1000/performance ≤1 detik |
| 052 | Dashboard SQL/status/count lokal; label fixture | Filter periode, by severity, aging/MTTR dan rekonsiliasi laporan belum lengkap; hitungan oracle SQL independen belum diuji |
| 053–054 | Outbox mock bounded backoff/dead/manual retry + unique delivery test | Tidak ada adapter CIF request/fallback/source attribute yang nyata bahkan mock; status dead ditampilkan literal, belum label operasional GAGAL-PERLU-TINDAKAN; retry audit belum atomik |
| 060–062 | 0.1.0 /version+footer, APP_COMMIT dari operator, legacy diff kosong | APP_COMMIT belum otomatis terikat SHA; semua layar state/loading/empty/error, keyboard, draft loss warning, responsive/contrast belum tervalidasi UX; login API 401 merender ulang sehingga draft berisiko hilang |

## Perbaikan delta yang selesai

Requirement REQ-003/008/015/032/034, AC-F1-015/016/041, Compliance 06 §6.2 A-01/E-05, UX 08 kritis 5. Source update membatalkan approval agar data yang berubah memerlukan review ulang. Semua aksi update termasuk assignment/escalation secara konservatif memicu invalidasi. Auto-close menggunakan transaksi tiket+audit+invalidasi+outbox; gagal audit tidak meninggalkan status sukses. Dua tes baru memeriksa stale version, reset checker/note, audit invalidasi, rollback kegagalan, idempotensi auto-close. Tidak mengubah skema v1 atau legacy. Risiko konservatif: tambahan pekerjaan checker; invalidasi tidak menggantikan snapshot/hash laporan versi lengkap.

## Deferred resmi vs gap F1

F2 resmi PM: problem/change, katalog penuh, email nyata, integrasi live Lark/CIF/backend tiket, retensi angka dan idle timeout final. Saved views diusulkan BA F2. Pemindaian malware belum tersedia. Angka SLA/jam/timeout lokal adalah asumsi.

Item tabel gap yang tercantum dalam acceptance F1 **bukan otomatis dipindah ke F2**: auto-escalation, config maker-checker, pinning SLA/reopen, rekonsiliasi, merge, CIF mock, metadata ekspor dan UX tetap gap F1 sampai PM memutuskan scope. Dokumen DEPLOY baseline menyebutnya batas milestone, bukan persetujuan penundaan scope.

## Prioritas berikutnya untuk PM

1. Atomicity audit seluruh mutasi tersisa, kontrol identitas bebas/berkas, correlation denial, matriks API langsung (keamanan).
2. SLA pinning/reopen, response+triage alerts, auto-escalation+queue; gabung dengan kebutuhan workflow inti dan uji oracle independen.
3. Rekonsiliasi dashboard/ekspor, config approval, gap lifecycle/merge dan state UX. Integrasikan temuan QA/UX baseline sebelum push.

## Review, provisioning, deploy dan rollback

Repo: docs/REVIEW.md (perintah seed akun sintetis melalui password input tersembunyi), docs/DEPLOY.md (runtime/data/backup/rollback), docs/PR_BODY.md (body exact draf PR lokal). URL http://127.0.0.1:3100/ serta /#intake; HTTP utama/health/version 200 pada proses fixture lokal. Browser tab gagal ERR_SOCKS_CONNECTION_FAILED / loopback ditolak, sehingga akses browser dan login reviewer belum terverifikasi. Jangan menyebut review UX selesai. Akun fixture sementara menggunakan password acak tak dicetak; reviewer harus provision DB baru menurut REVIEW.md agar mengetahui password sendiri. Jangan mengirim password chat. NODE_ENV=development eksplisit diperlukan karena environment bawaan production; production fixture guard terbukti menolak percobaan awal tanpa override.

Tidak ada push/PR remote, merge, deploy automation atau data live berubah. PM menyelesaikan pemetaan Browser tab ke fixture yang didukung sebelum UX. Rollback delta ke 2a68ccb dengan proses dihentikan dan DB disalin setelah backup; schema v1 tetap, approval yang dibatalkan tidak dikembalikan otomatis. Rollback mengembalikan celah invalidasi baseline.

Sumber internal: PM 04 ADR/Fase; BA 08 §4/7; UX 07 §3/4 dan 08 kritis 1–5; Compliance 06 §6.2 A-01/E-05 dan §6.4. Rujukan S08/S09 melalui adaptasi acceptance lokal, bukan verifikasi hukum atau klaim pengesahan sumber. Detail sumber snapshot belum diverifikasi ulang ke kondisi live.

## DEV-F1-11 — Delta atas 22747b9 (menggantikan status lama yang bertentangan)

Input PM: BUG-QA-004 pada QA11; keputusan invalidasi konservatif untuk reply/internal note/lampiran; UX09-01. Requirement REQ-001/008/015/032/034, AC-F1-001–010/015/041/050. Status **implemented + tested Developer**, bukan penutupan QA independen.

- Lampiran: insert+audit+versi tiket+invalidasi approval+outbox sekarang satu transaksi. Pesan reply/internal memakai kebijakan sama. REVIEWED/DISETUJUI_MANUSIA menjadi DATA_DRAFT, checker/note NULL, report version +1 tepat sekali; tiket version +1; stale approval 409. Reply mengisi first_response sekali, note/lampiran tidak mengubahnya. Bahkan internal note membatalkan approval karena evidence pelaporan.
- Fault injection HTTP pada 3 mutasi × 2 status × 3 titik gagal (audit mutasi, audit invalidasi, insert outbox) membuktikan snapshot tickets/reports/messages/attachments/audit/outbox identik sebelum/sesudah gagal. Sukses dan stale approval juga diuji.
- UX09-01: field merchant/PIC/taxonomy dari 22747b9 dipertahankan; validasi email/WhatsApp, judul maksimum 200 tanpa pemotongan diam-diam, kronologi wajib maksimum 10000, kontak maksimum 254. Lampiran publik opsional maksimum 5/total 5 MB, allowlist/magic/pola identitas sama dengan upload operator. Intake idempoten menyimpan payload; worker menghasilkan tiket+lampiran+audit+outbox atomik, replay/job ulang tidak menggandakan berkas. Uji audit worker gagal lalu retry PASS. UI tidak menyimpan File ke draft atau Web Storage; setelah navigasi berkas perlu dipilih ulang.
- Scope akses internal F1: **satu service desk DOB**. Tidak ada isolasi per tim/merchant; proyeksi masked L3/CMR/AUD dan penolakan raw attachment tetap berlaku. Ini batas implementasi, bukan klaim multi-tenant.

Bukti `docs/evidence/developer-evidence-atomicity.tap`; `npm test`, Node 24.22.1. Browser/a11y/expiry/navigation UX09-01/04 dan QA independen SHA baru masih NOT RUN. BUG-QA-001–003 sudah regresi Developer pada koreksi sebelumnya, belum dianggap ditutup independen oleh dokumen ini.

Audit gap F1 tersisa: pengelolaan role/config maker-checker; merge; SLA calendar/version pinning dan reopen; response/triage alerts dan escalation queue; pagination/filter/saved state; dashboard periode/severity/aging/MTTR; metadata manifest ekspor; CIF mock; audit atomicity login/logout/manual retry; correlation denial; seluruh matriks endpoint×role/IDOR; pemindaian malware/minimisasi binary; performance/200 oracle SLA; UX09-06..15 dan runtime/visual. Preview rekonsiliasi periode sudah ada sejak 22747b9 tetapi sumber eksternal tetap unavailable/incomplete. Gap tersebut tidak otomatis ditunda ke F2.

Deferred resmi tetap: problem/change, katalog penuh, email/integrasi live, retensi final sesuai keputusan PM. Tidak push/deploy/PR pada task koreksi ini. Tidak mengubah file legacy atau jaringan fixture.

## DEV-F1-12 — Delta atas 4e80494 (menggantikan gap auth/retry/draft terkait)

Requirement REQ-001/001b/008/030/032/040; AC-F1-002/015/030/053/061. QA12 BUG-QA-006/007/008 dan UX10 D10-01..05 **implemented + tested Developer**, belum penutupan reviewer independen. Login rotasi+audit, logout revoke+audit dan retry outbox+audit atomik; cookie hanya setelah commit. Fault logout mempertahankan sesi (keputusan PM), UI menyatakan kegagalan dan mempertahankan draft. Intent intake dan field RAM bertahan saat in-flight/navigasi/retry sampai receipt; lost response diuji HTTP, key sama/count1. Schema retry, dirty logout confirmation, restored filter initial query, preview period/stale late-response guard, macro append+RAM dan ARIA cleanup ditambahkan.

Bukti: docs/evidence/DEV-F1-12-auth-draft.md dan developer-auth-draft.tap, 26/26 PASS. Empat tes renderer memakai node:vm+DOM double, bukan browser visual/a11y. Delta tidak menyediakan URL persisten terikat SHA; Browser tab/login/viewport/keyboard tetap NOT RUN. Lampiran draft tidak disimpan, harus dipilih ulang; payload replay yang berbeda ditolak409. Batas reset RAM saat reload/account berbeda tetap berlaku. Gap SLA/config/merge/pagination/dashboard/manifest/CIF dan UX lain tidak otomatis ditutup. Tidak ada perubahan skema/legacy/deploy/jaringan atau push. Rollback delta 4e80494 setelah backup+stop mengembalikan bug yang diperbaiki.

## DEV-F1-13 — Delta lifecycle dan UX terbatas

UX11-01 **implemented + tested Developer**: epoch logout sukses/account switch menonaktifkan callback/form lama; delayed success/rejection tidak memulihkan draft/intents lama. Logout fault dan expiry akun semula tetap recovery; receipt hilang dengan key sama/satu intake tetap PASS. UX09-13 skip handler serta bagian KB query restore/error/retry UX09-12 diperbaiki. Bukti baseline FAIL → koreksi **32/32 PASS**, output/hash/perintah/rollback pada docs/evidence/DEV-F1-13-lifecycle.md. Tes renderer DOM double; browser nyata/visual/a11y dan QA independen delta **NOT RUN**. UX09-12 keseluruhan tetap PARTIAL; UX09-01..11/14/15 mempertahankan open/partial review UX11 (termasuk UX09-04 browser). Gap F1 lainnya tidak dipindahkan ke F2. Tidak push/deploy/ubah legacy/jaringan.

## DEV-F1-NEXT-01 — SLA pinning/reopen

CFG-04/05 (BA08 §7), AC-F1-022/027 dan fondasi AC026: snapshot kalender/target semua severity per tiket, version hash, severity/alert memakai snapshot; interval Teratasi/Ditutup dikecualikan setelah reopen, reopen_count dan first_response dipertahankan. Migrasi aditif/backfill audit dengan label legacy-baseline/history-incomplete; belum membuktikan kalender historis yang tidak disimpan. Developer 39/39 PASS termasuk 200 pembanding konsumsi menit dan rollback backup dengan domain 27aafb0. Evidence docs/evidence/DEV-F1-NEXT-01.md dan .tap. QA independen delta serta browser NOT RUN. Due/history, oracle AC020 lengkap, approval config dan eskalasi masih open. Delta commit lokal, tanpa push; PR #1 tetap DRAFT.

## DEV-USER-23 — bootstrap dan pengguna (baseline68476bb)

USER01..08 di docs/evidence/DEV-USER-23.md: implemented/tested Developer,44/44 PASS. Bootstrap CLI sekali pada exact initialized existingDB dengan password stdin; ADM list/detail/create/update/deactivate/reactivate/reset; password reset/create wajib ganti, revoke sessions, last active ADM protected transaction+DBtrigger, account mutation+audit atomik. UI dan force-change tanpa secret RAMdraft. Bukti HTTP/SQLite/fault/race/migration+DOM; native browser/QA independen belum diuji. docs/USERS.md berisi provisioning/migration/rollback. Tidak memprovision port3000. Approval CMR perubahan role/config belum diselesaikan oleh ADM CRUD, tetap gap PM; F1 lainnya tidak ditutup. Downgrade backend lama pada DB baru dengan login terbuka tidak aman karena active/must_change_password diabaikan; stop/backup/maintenance atau forward fix.
