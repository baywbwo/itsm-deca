# DEV-F1-12 — audit auth/retry dan draft intake

Owner Developer, reviewer PM/QA/UX; baseline immutable 4e80494. Input PM assignment 13bfbbec-1fcf-478a-b4b4-3d5145d6abbf; QA12 bagian BUG-QA-006/007/008; UX10 bagian D10-01..05. Requirement REQ-001/001b/008/030/032/040, AC-F1-002/015/030/053/061; kontrol Compliance06 §6.2 A-01. Acuan adalah acceptance internal; tidak memverifikasi regulasi baru.

## Hasil implementasi

- Login: revoke sesi lama (bila cookie ada), insert sesi baru dan audit login satu transaksi. Audit gagal → HTTP500, cookie baru tidak dikirim, sesi lama tetap valid; recovery → rotasi cookie dan pencabutan sesi lama.
- Logout sesuai keputusan PM: revoke+audit satu transaksi. HTTP200 dan cookie Max-Age=0 hanya setelah keduanya committed. Fault → HTTP500 tanpa Set-Cookie, sesi tetap valid; UI menyatakan Keluar gagal/status belum dikonfirmasi, tidak menghapus draft. Kegagalan transport setelah commit berbeda: klien tidak dapat memastikan status server; UI meminta retry, tidak menjanjikan sesi tetap valid pada semua kegagalan jaringan.
- Retry outbox: perubahan state/attempts/next_at dan audit satu transaksi. Fault mempertahankan snapshot outbox lama; recovery satu audit, retry berikutnya pada pending ditolak409. Kontrak error lama tetap dipertahankan.
- D10-01: state RAM menyimpan intent key terpisah dari renderer form, melekat pada account/route/form dan bertahan sampai receipt berhasil atau logout sukses. Draft field intake tetap di RAM selama request in-flight. Navigasi/retry sesudah respons diterima server tetapi hilang di klien memakai key sama → replay, bukan intake baru. Tidak ada password/File/base64 lampiran dalam draft. Setelah navigasi, lampiran harus dipilih ulang sama persis; payload berbeda pada key yang sudah diterima ditolak409, bukan diam-diam membuat intake lain. Recovery lampiran lewat browser nyata masih perlu diuji.
- D10-02: dirty logout memakai konfirmasi Batal/Keluar sebelum API; gagal mempertahankan RAM; sukses membersihkan seluruh draft+intent. Filter pencarian juga dihitung konservatif sebagai state belum dibuang.
- D10-03: schema error ditangkap, region error menetap dan Coba lagi; draft RAM tidak dihapus.
- D10-04: initial query memakai field filter yang dipulihkan; filter Cari tetap tersimpan setelah submit. Preview menampilkan periode hasil dan waktu; perubahan field menandai basi, dan respons request lama tidak menimpa state basi (generation guard).
- D10-05: macro menambahkan teks tanpa menimpa tulisan, memicu input untuk snapshot RAM; error sebelumnya dan relasi ARIA dibersihkan saat koreksi/input/submit/error berikutnya. Bantuan yang bukan ID error tetap dipertahankan. Bantuan file menyebut pilih ulang setelah navigasi.

## Verifikasi aktual

Node 24.22.1; `npm test > docs/evidence/developer-auth-draft.tap 2>&1`: **26/26 PASS**, fail0/skip0. Suite lama tetap berjalan. `node --check` JS berubah, `git diff --check`, diff legacy terhadap 37a9bba kosong. Scan terbatas pola secret pada delta, bukan jaminan exhaustive.

Fault regression HTTP+SQLite di app/test/product.test.js: trigger audit gagal pada login/logout/retry; snapshot sessions/outbox; header cookie, old/new session usability dan recovery audit tunggal. Callback waktu last_seen dapat berubah oleh request terautentikasi; invariant logout adalah kehadiran/validitas sesi, bukan menganggap middleware last_seen tidak berjalan.

Empat tes app/test/ui.test.js mengeksekusi renderer app.js aktual dalam node:vm dengan DOM double minimal. D10-01 menggunakan server HTTP nyata dan SQLite disposable: server menerima intake, respons sengaja hilang; form direkonstruksi, key sama, count DB tepat1, receipt berhasil membersihkan RAM. Juga snapshot draft saat request masih in-flight. Schema500 lalu recovery mempertahankan field. Tes lain memeriksa confirm Batal/gagal/sukses logout, restored filter query, periode preview+late-response, macro append+restore dan ARIA cleanup. DOM double bukan browser engine, bukan bukti layout/keyboard/focus/kontras/native dialog/FileReader/a11y.

## Gap runtime dan batas

Tidak menyalakan fixture persisten baru, tidak mengklaim URL aktif terikat SHA ini. Endpoint HTTP tes memakai port dinamis loopback dan ditutup setelah tes. URL sebelumnya tidak menjadi bukti build sekarang; keterjangkauan Browser tab dan login reviewer tetap NOT RUN pada delta ini. Tidak membuka publik, mengubah jaringan SCO, memakai akun live atau melakukan push/deploy. QA menguji 4e80494 paralel; hasil itu tidak otomatis menutup bug pada SHA koreksi ini. QA perlu retest SHA baru dan UX perlu browser sesungguhnya D10-01..05.

Gap F1 lain pada STATUS_F1.md tetap terbuka. Idempotency draft hanya dalam umur halaman/account yang sama; reload/tab tutup menghapus RAM sesuai label. Portal multi-tenant, exactly-once lintas halaman/perangkat, pemulihan File, serta idempotensi POST pesan/operator attachment belum ditambahkan. DOM race antar navigasi async secara umum bukan diselesaikan menyeluruh oleh guard preview ini.

## Rollback

Tidak ada perubahan skema. Hentikan proses, backup konsisten sesuai DEPLOY.md, gunakan kode 4e80494 terhadap salinan DB; approval/data tidak dipulihkan otomatis. Rollback mengembalikan bug atomicity auth/retry dan kehilangan intent navigasi. Session/draft aktif harus dianggap berakhir saat restart/refresh; jangan menyalin draft/secret ke artifact. Draf PR lokal diperbarui; publikasi menunggu PM mengintegrasikan review.
