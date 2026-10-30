# DEV-SLA-FIX-21 — QA20-D01

Requirement: CFG-04/05, AC-F1-020/022/026/027; BA 08-acceptance-f1 §7 dan 03-sla-jam-kerja-eskalasi §3.5. Basis 6895c86. Reviewer berikutnya QA independen dan PM. PR #1 tetap DRAFT; tidak push/merge/deploy.

Replay migrasi kini memeriksa creation evidence, urutan waktu dalam batas created/updated, transisi legal, kesinambungan before/after status dan kecocokan status akhir dengan row tiket. Auto-close ikut replay. Payload kosong/rusak dan status hilang diberi legacy-history-incomplete. Audit asli tidak diedit; resolved hanya diperbaiki bila replay lengkap. Origin legacy-baseline tetap menyatakan kalender historis tidak tersedia, bukan kalender historis terverifikasi. history_complete hanya berarti replay lolos validasi yang tersedia, bukan bukti audit tak pernah kehilangan rangkaian siklus lengkap yang tak terdeteksi.

Snapshot history_revision=2 memungkinkan satu kali validasi ulang baseline hasil migrasi 6895c86; tiket created dan revision2 tidak dimigrasi ulang. Tidak mengisi interval yang hilang. Nilai used_minutes dan remaining_minutes untuk history incomplete menjadi null, history_complete=false serta history_warning eksplisit. UI daftar/detail menampilkan SLA tidak terverifikasi; dashboard memiliki sla_unverified dan tidak menghitung null sebagai breach. Worker melewati threshold SLA yang tidak pasti. Response tetap memakai timestamp first_response dan kalender baseline dengan peringatan asal kalender; bukan klaim kalender historis. Export CSV saat ini tidak memuat angka SLA, sehingga tidak mengekspor konsumsi yang tidak pasti. Due/history, approval config, eskalasi tetap open.

## Verifikasi aktual

Node disposable, tanpa data/live akun. Salinan git archive basis ditimpa hanya file delta; tanpa .git. Perintah `node --test app/test/*.test.js`: 40 PASS, 0 FAIL, 0 SKIP. Setelah tambahan label counter dashboard, `node --test app/test/ui.test.js`: 9 PASS. DOM double bukan browser visual.

Repro QA20 independent.mjs disalin tanpa modifikasi ke root archive: 7 PASS, 0 FAIL (termasuk 240 oracle boundaries, fault audit/outbox, complete/missing/partial/restart). Complete tetap used=15, reopen=1, legacy-baseline. Missing/partial: null konsumsi/sisa, history incomplete. Tes tambahan malformed/gap/zero-event; tiket Baru tanpa status-event tetap baseline jika creation evidence ada. Audit tetap identik, restart idempoten dan tidak ada threshold alert untuk history incomplete. Suite mempertahankan tes backup/downgrade/reupgrade dan reopen fault rollback.

Tes recovery kini memakai fixture source baseline nonsecret `app/test/fixtures/domain-27aafb0.txt`, salinan byte-identik git show 27aafb0:app/domain.js; SHA256 335893a78a1a298f2bd5211e5bfad09a618361749daca937f0ee2900518d74aa ditegaskan dalam tes. Tetap menjalankan domain sebelumnya pada backup serta upgrade ulang, tanpa membutuhkan Git/history.

## Risiko dan rollback

Kontrak null baru mengharuskan konsumen tidak mengubah null menjadi angka 0; UI produk ditangani. Validasi tidak dapat membuktikan history yang dihapus secara konsisten tanpa jejak; perlu rekonsiliasi sumber eksternal sebelum klaim historis. Tidak ada klaim SLA resmi.

Rollback operasional mengikuti DEV-F1-NEXT-01 dan DEPLOY.md: stop app/worker, backup konsisten sebelum migrasi, uji restore pada salinan, pulihkan backup pra-upgrade dan source sebelumnya; jangan DROP kolom atau mengembalikan source saja pada DB aktif. Simpan write pasca-backup untuk rekonsiliasi. Tidak melakukan migrasi live.

QA retest SHA baru: belum dijalankan. Browser/keyboard/responsif tetap NOT RUN karena akses privat platform. ACCESS-LOGIN-01 dan file untracked lain tidak disentuh.
