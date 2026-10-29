# DOB ITSM Platform 0.1.0 — kontrak aplikasi

Keputusan ADR-001 PM, acceptance BA 08, UX 07 dan kontrol Compliance 06 menjadi acuan. Runtime lokal yang diuji: **Node 24.22.1**, SQLite bawaan Node (API experimental). Nol dependency npm. `npm test`, `npm start`. Default bind **127.0.0.1:3100**; kode lama dan deploy automation tidak diubah. Tidak menjalankan deploy/deploy.sh.

## Data dan konfigurasi

`APP_DB` menunjuk SQLite pada direktori persisten privat di luar web root (default app/data/itsm.sqlite). Direktori data mode 0700; jalankan sebagai user layanan khusus dengan umask 077. Termasuk data kontak, pesan dan lampiran; lindungi backup dengan akses yang sama. Tidak ada penghapusan otomatis atau klaim enkripsi at rest. `config/taxonomy.json`: S1–S4, jam Senin–Jumat 08–17 WIB, hari libur, status pengaduan internal; semua angka merupakan asumsi lokal, bukan ketentuan regulator. Konfigurasi runtime belum dapat diubah dari UI; jangan edit sewaktu proses berjalan. Belum ada endpoint kelola pengguna/role/config, sehingga perubahan tersebut fail-closed.

`HOST`, `PORT`, `APP_DB`, `APP_COMMIT`, `COOKIE_SECURE=1` untuk TLS. Reverse proxy harus meneruskan ke layanan Node ini, membatasi request body, dan mempertahankan host/origin. HTTPS wajib untuk akses non-loopback. Endpoint `/healthz` dan `/version` tersedia; identitas commit dipasok APP_COMMIT oleh operator. Jangan berasumsi push mengalihkan VM lama ke Node: aturan auto-deploy eksternal belum terlihat di repo. Push dianggap dapat memicu deployment; pemetaan runtime tujuan belum diverifikasi. Port lama 3000 dan Python tidak otomatis digantikan. PR tidak boleh dianggap sebagai rilis produksi atau migrasi data live.

## Fixture lokal opt-in

Hanya gunakan DB disposable, `NODE_ENV` selain production dan `APP_FIXTURES=1`. Masukkan password sementara ≥14 karakter melalui environment lokal/terminal tersembunyi, bukan source atau chat:

```sh
read -rs -p 'Password fixture lokal: ' FIXTURE_PASSWORD
export FIXTURE_PASSWORD
APP_FIXTURES=1 APP_DB=/tmp/dob-fixture.sqlite npm run fixture
unset FIXTURE_PASSWORD
APP_FIXTURES=1 APP_DB=/tmp/dob-fixture.sqlite APP_COMMIT=$(git rev-parse --short HEAD) npm start
```

Akun sintetis `opr@example.test`, `sup@example.test`, `l2@example.test`, `l3@example.test`, `cmr@example.test`, `aud@example.test`, `adm@example.test`. Semua memakai password yang operator masukkan. Data fixture diberi is_test dan dikecualikan dari ekspor; intake pada server mode fixture juga ditandai is_test. Jangan hidupkan fixture pada VM production. Tidak ada password default maupun akun production yang disediakan. Provisioning pengguna nyata memerlukan proses admin tersendiri dengan dual control (belum tersedia pada milestone ini).

## Backup, restore dan rollback lokal

`APP_DB=/path/source.sqlite npm run backup -- /path/backup.sqlite` memakai SQLite VACUUM INTO untuk snapshot konsisten termasuk WAL, menolak overwrite, lalu integrity_check. Backup memuat lampiran yang disimpan dalam SQLite. Uji produk membuat DB disposable, backup, membuka kembali snapshot, memeriksa integrity_check dan jumlah tiket.

Restore: hentikan proses Node, simpan DB aktif + file WAL/SHM sebagai satu set karantina, salin snapshot ke path baru, periksa integrity_check, lalu mulai proses dengan APP_DB menunjuk salinan baru. Jangan menimpa DB yang masih terbuka. Rollback kode: hentikan Node baru, pilih commit aplikasi sebelumnya, mulai dengan snapshot pra-upgrade yang disalin; jangan jalankan kode lama terhadap skema baru tanpa pemeriksaan kompatibilitas. Rilis ini schema user_version=1 dan hanya membuat tabel baru pada DB aplikasi baru. Tidak ada migrasi/import otomatis data lama; live tidak disentuh. Baseline 37a9bba tidak mempunyai Node app: rollback awal berarti menghentikan layanan Node tambahan dan mengembalikan routing operator sebelumnya. Konfigurasi VM/routing tersebut belum teruji.

## Gate sebelum push

- npm test lulus pada Node pin; git diff file legacy terhadap 37a9bba kosong.
- QA independen pada commit stabil; BA skenario kerja; review UX browser aktual; review kontrol privasi dan release PM.
- Secret scan diff; fixture off untuk deploy; konfigurasi HTTPS/cookie/data path ditinjau, legacy tidak diubah.
- Dokumen backup/restore/rollback dan batasan ditinjau; tidak ada eksekusi deployment atau merge main.

## Batas milestone dan F2

Adapter hanya mock lokal dengan retry terbatas/dedup key dan status dead terlihat. Tidak ada panggilan Lark/CIF/backend tiket nyata. Tidak ada email terkirim atau BI submission. Lampiran belum dipindai malware, hanya validasi ekstensi/signature dasar dan attachment download. Laporan merupakan data internal, template/status resmi belum diverifikasi. Retensi final, pengelolaan peran/config maker-checker, auto-escalation penerima, merge, saved views, SLA response alerts, rekonsiliasi per periode dashboard, akun nyata dan migrasi data lama belum lengkap pada milestone ini. **Jangan nyatakan seluruh F1 selesai atau siap produksi.**

Dugaan kebocoran data: hentikan ekspor, catat referensi tiket dan waktu tanpa menyalin data pribadi, eskalasi ke Compliance untuk penilaian dan tindakan manusia. Aplikasi tidak mengirim pemberitahuan regulator otomatis dan tidak menetapkan tenggat hukum.

## Delta DEV-F1-09

Perubahan sumber tiket membatalkan approval laporan ke DATA_DRAFT; stale approval ditolak oleh version. Auto-close sekarang atomik dengan audit, invalidasi dan outbox. Skema tetap v1, tidak perlu migrasi. Rollback delta: hentikan proses, buat snapshot konsisten, jalankan baseline 2a68ccb terhadap salinan DB; status approval yang telah dibatalkan tidak dipulihkan otomatis. Backup pra-delta hanya dipakai jika pemulihan seluruh data memang dimaksudkan; jangan hilangkan perubahan operasional hanya untuk memulihkan approval. Baseline belum melindungi invalidasi ini sehingga rollback mengembalikan risiko tersebut. Instruksi review/provisioning aman ada di REVIEW.md.

### Koreksi F1 setelah b8087ce
Startup menambah kolom `tickets.intake_fields` (JSON teks) dan `resolution_cause` bila belum ada, tanpa mengubah baris lama. Backup SQLite sebelum menjalankan build pada database persisten; upgrade additive diuji pada fixture. Rollback kode ke b8087ce dapat membaca database dengan kolom tambahan; untuk rollback skema persis gunakan backup pra-upgrade dalam keadaan proses berhenti. Jangan hapus kolom pada data operasional.
Kontrak intake baru mewajibkan `merchant` (1–120), `pic` (1–80), `category` dari katalog dan `type` Incident/Service Request selain field lama. GET `/api/intake-schema` menyediakan katalog publik. Intake tertunda lama tetap diproses dengan fallback metadata kosong. Resolusi memerlukan `resolution_cause` (1–1000 karakter, teks internal; bukan kode regulator tervalidasi).
POST `/api/reconciliation` menerima from/until WIB yang sama dengan ekspor. Panel hanya membuktikan rekonsiliasi database lokal; Zammad/Lark belum tersedia dan keseluruhan selalu bertanda tidak lengkap. Tidak ada klaim integrasi nyata.
Draft SPA berada di memori halaman; logout menghapusnya, reload/menutup halaman menghilangkannya dengan peringatan browser. File unggahan/password tidak disimpan sebagai draft. Uji browser untuk perpindahan halaman dan expiry masih diperlukan sebelum acceptance UX.

### DEV-F1-11 rollback compatibility

Tidak ada schema baru. Intake publik sekarang dapat membawa attachment base64 (maks 5/total 5 MB); worker versi lama mengabaikan field ini. Sebelum rollback ke 22747b9, hentikan intake baru dan drain pending intake dengan worker versi baru, atau restore backup konsisten sebelum fitur dipakai. Backup/restore tetap mengikuti prosedur di atas. Jangan hapus audit/data langsung. Approval yang dibatalkan membutuhkan review ulang; rollback tidak mengembalikannya. Upload belum dipindai malware; payload intake dan BLOB menyebabkan duplikasi storage yang perlu kapasitas/retensi. Versi ini tetap lokal sampai QA/UX/release gate lulus.
