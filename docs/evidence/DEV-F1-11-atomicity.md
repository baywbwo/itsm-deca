# DEV-F1-11 — BUG-QA-004 dan evidence invalidation

Owner Developer; reviewer PM/QA/UIUX. Baseline 22747b9, branch pilot/itsm-revamp-v2. Input: PM handoff; preparation-v2/05-qa/11-verifikasi-b8087ce.md; 04-ux/09-review-2a68ccb.md dan 07-handoff-implementasi.md. REQ-001/008/015/032/034; AC-F1-015/041/050. Adaptasi kontrol audit lokal, bukan klaim regulasi.

Implementasi dan audit gap aktual: docs/STATUS_F1.md bagian DEV-F1-11. Perintah verifikasi: `npm test > docs/evidence/developer-evidence-atomicity.tap 2>&1`, `node --check app/public/app.js`, `node --check app/domain.js`, `node --check app/server.js`, `git diff --check`. Tes produk memakai HTTP loopback dan SQLite disposable, bukan live. Runner fault injection berada di app/test/product.test.js, dua tes terakhir. Hasil final dicatat dalam TAP. Tidak memakai atau mengubah archive reviewer.

Uji atomicity bukan sekadar jumlah attachment: snapshot seluruh enam store dibandingkan untuk 18 kombinasi kegagalan. Sukses memeriksa report/ticket version, checker/note, invalidation audit tunggal, event outbox dan stale checker 409. Intake attachment memakai fault audit worker dan job ulang. Ini uji Developer; QA independen masih diperlukan.

Risiko: invalidasi konservatif menambah review; POST pesan/lampiran belum memiliki idempotency key (retry setelah respons sukses hilang masih dapat duplikasi). Public attachment memakai payload base64 dalam intake dan BLOB setelah worker; ada duplikasi storage, maksimum 5 MB total/request dan rate limit yang ada, belum quota global/retensi final. Magic bytes bukan malware scan atau jaminan PII binary. Berkas draft tidak dipertahankan sesudah navigasi. URL fixture browser masih unresolved, tidak membuka publik/mengubah jaringan.

Rollback: hentikan proses, backup SQLite sesuai docs/DEPLOY.md, kembalikan kode ke 22747b9 bila terpaksa. Skema tidak berubah; kode lama tidak memproses attachment intake pending sehingga drain intake memakai versi baru sebelum rollback atau restore backup konsisten sebelum fitur ini. Jangan hapus audit/berkas langsung; approval yang sudah dibatalkan tidak otomatis dikembalikan. Rollback mengembalikan BUG-QA-004 dan gap invalidasi lama. Tidak push/deploy.

Hasil akhir aktual: Node v24.22.1, **21/21 PASS**, 0 fail/skip, exit 0. Pemeriksaan sintaks ketiga file JS dan diff whitespace PASS; diff index.html/api/deploy kosong. Checkout baseline tidak mempunyai perubahan dari penulis lain saat mulai. Tidak ada perubahan schema atau file legacy.
