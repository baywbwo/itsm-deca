# Changelog

## 0.1.0 — F1 milestone lokal (belum release)
- Tambah domain Node 24.22.1 + SQLite dan SPA DOB ITSM Platform tanpa dependensi npm.
- Intake asinkron idempoten, autentikasi/sesi, RBAC, triase/lifecycle/penugasan, percakapan dan lampiran.
- SLA menit kerja dan jeda, KB/macro fixture, dashboard, laporan maker-checker dan CSV masked, outbox retry/dedup mock.
- Uji HTTP produk dan backup SQLite lokal; file aplikasi/deploy legacy dipertahankan.
- QA/UX independen dan rekonsiliasi PM masih diperlukan. Gap milestone tercatat di docs/DEPLOY.md.
- Koreksi QA-001/002/003: masking telepon berformat dan proyeksi laporan/CSV, validasi tanggal kalender ketat.
- Koreksi UX09 prioritas tinggi: field intake merchant/PIC/taksonomi, penyebab resolusi, pratinjau rekonsiliasi lokal dengan sumber eksternal tidak tersedia, draft sementara di memori.
- Regresi revisi source yang sudah disetujui pada preview/CSV dan upgrade SQLite additive; acceptance browser/QA independen tetap belum disahkan.

### Koreksi DEV-F1-11 (lokal, belum dirilis)
- Atomikkan evidence lampiran/pesan dengan audit, versi sumber, invalidasi approval dan outbox; rollback fault injection.
- Lengkapi lampiran intake publik dan validasi kontak/judul; worker intake atomik dan replay aman.
- Tegaskan batas akses satu service desk DOB dan gap F1 yang belum selesai.
