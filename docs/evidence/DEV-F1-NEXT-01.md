# DEV-F1-NEXT-01 — pinning kebijakan dan reopen

Owner Developer; reviewer berikutnya QA independen dan PM. Basis 27aafb05386d1951fb05d0c2c965f9983eefa40e pada pilot/itsm-revamp-v2. Target publikasi tetap PR https://github.com/baywbwo/itsm-deca/pull/1 (DRAFT); delta ini hanya commit lokal, tidak push/merge/deploy.

## Kontrak dan hasil

Sumber: BA 02-requirements/08-acceptance-f1.md §7 CFG-02/04/05, AC-F1-020/021/022/026/027; 03-sla-jam-kerja-eskalasi.md §3.5 A1–A8. AC-023..025 tetap parsial/open; perubahan ini tidak mencakup workflow approval konfigurasi atau eskalasi. Tidak menyatakan seluruh AC-020 selesai.

- Tiket baru menyimpan JSON snapshot jadwal, libur, target semua severity, zona dan asumsi. calendar_version adalah SHA256 isi kebijakan; perubahan config file hanya memengaruhi tiket yang dibuat berikutnya. Tidak ada endpoint konfigurasi baru atau bypass maker–checker. Perubahan severity menggunakan target dari snapshot yang sama; worker alert resolusi juga memakai target terpin.
- Resolusi mengurangi menit Menunggu Pelapor dan menit interval Teratasi/Ditutup sebelum reopen. Reopen count bertambah atomik bersama status/audit/outbox, first_response tetap. Transisi Teratasi→Ditutup mempertahankan resolved terakhir. Auto-close tetap 72 jam kalender dan tidak menutup tiket yang sudah reopen.
- Jadwal tetap Asia/Jakarta, interval setengah terbuka, presisi submenit. Tidak menambah jadwal berbeda per severity di luar kontrak default.

## Migrasi dan batas evidence historis

Migrasi aditif SQLite: sla_policy TEXT, stopped_minutes REAL default 0, reopen_count INTEGER default 0. Kolom dan backfill dalam satu transaksi IMMEDIATE, dijalankan hanya untuk sla_policy NULL; restart idempoten. Tidak ada penghapusan data/history dan format user_version=1 tetap aditif.

Tiket lama belum pernah menyimpan versi konfigurasi; baseline config saat migrasi diberi policy_origin=legacy-baseline, bukan klaim versi historis. Replay event status audit (created/id) memulihkan interval resolved dan reopen_count; memperbaiki cap resolved tiket tertutup bila event Teratasi tersedia. Legacy masker menyimpan [masked] tanpa kutip pada angka timestamp di detail audit; parser migrasi menormalisasikan nilai itu ke null dan menggunakan cap waktu kolom event. Audit asli tidak diedit.

Riwayat yang tidak dapat diparse atau interval yang tidak dapat ditentukan diberi policy_origin=legacy-history-incomplete. Hasil historis semacam itu perlu rekonsiliasi sebelum pelaporan; migrasi tidak mengarang interval yang hilang. Kalender historis/riwayat yang telah hilang tidak dapat dibuktikan dari source ini.

## Verifikasi

Node 24.22.1, disposable SQLite di tmp; tidak ada akun/DB production. Perintah: `node --test app/test/*.test.js`. Output: DEV-F1-NEXT-01.tap. 39 PASS, 0 FAIL, 0 SKIP (32 regresi + 7 tes delta).

Tes delta mencakup perubahan defaults setelah snapshot, severity memakai versi lama, worker tidak terpicu target baru, pause lintas weekend, Teratasi→Ditutup→reopen berulang, first_response, detik setengah menit, libur lintas tahun, audit fault rollback seluruh tiket dan outbox, migrasi/restart/integrity, perbaikan resolved legacy dan flag history tidak terbaca. 200 kasus kalkulator pembanding menggunakan interseksi jendela harian (algoritme berbeda dari loop menit produksi); ini oracle Developer untuk konsumsi menit, bukan 200 skenario due/severity/pause oracle QA lengkap AC-020. Due timestamp dan histori due lama belum ditambahkan.

Backup pra-migrasi disposable dengan VACUUM INTO, dibuka oleh domain asli dari `git show 27aafb0:app/domain.js` dalam direktori terpisah; tiket terjaga dan integrity_check ok. Kemudian upgrade ulang backup tersebut juga lulus. Fault injection reopen membuktikan versi, akumulasi jam dan outbox tidak berubah saat audit gagal.

QA independen SHA delta, browser/keyboard/responsif dan fixture persisten baru: NOT RUN. Tidak mengulang akses browser yang terblokir. Hasil suite HTTP baseline tidak menjadi bukti runtime browser baru.

## Deploy compatibility, risiko, rollback

Runtime/deploy contract tetap docs/DEPLOY.md, tanpa dependensi tambahan, listener, automation atau perubahan live. Snapshot menambah ukuran setiap row tiket. Backfill membaca audit per tiket dan menghitung menit historis: pada data besar perlu staging/cadangan serta ukur durasi sebelum rollout. Konfigurasi default berbeda sebelum migrasi tidak dapat dipulihkan otomatis; jangan mengklaim akurasi historis tanpa rekonsiliasi.

Sebelum upgrade nyata: hentikan app/worker, buat backup konsisten via app/backup.js, catat SHA dan integrity_check; coba upgrade pada salinan terlebih dahulu. Rollback aman: hentikan proses baru, simpan DB baru untuk rekonsiliasi, pulihkan backup pra-upgrade ke lokasi APP_DB, jalankan 27aafb0 dan cek health/data sesuai DEPLOY.md. Jangan salin SQLite aktif/WAL secara parsial. Restore backup kehilangan write pasca-backup sehingga perlu rekonsiliasi terkontrol. Tidak DROP kolom pada DB aktif; downgrade source saja mengaktifkan kembali bug reopen dan mengabaikan snapshot, sehingga bukan rollback operasional yang dianjurkan.

Gap tetap: due timestamp/history, oracle penuh AC020, konfigurasi approval AC025/026 end-to-end, alert respons/triase dan siklus/escalation queue, serta gap lain STATUS_F1. Browser terblokir platform, PR DRAFT, tidak menyatakan release siap.
