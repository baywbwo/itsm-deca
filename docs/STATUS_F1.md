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
