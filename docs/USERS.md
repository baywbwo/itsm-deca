# Bootstrap dan pengelolaan pengguna — DEV-USER-23

Implementasi lokal, bukan provisioning instance3000. Hanya operator yang memiliki izin spesifik pada host/database tujuan boleh menjalankan CLI ini setelah resource dan approval dikonfirmasi. Tidak ada password default, public bootstrap atau reset otomatis.

## Bootstrap sekali

Gunakan Node24.22.1, checkout deploy terverifikasi, database aplikasi yang sudah ada dan pernah diinisialisasi (tabel users ada), path absolut exact, backup konsisten dan user layanan yang berwenang. CLI menolak path hilang/direktori/database tanpa tabel users, existing ADM (termasuk nonaktif), email existing atau marker bootstrap. CLI menambah kolom pengguna bila diperlukan seperti startup domain; ini mutasi, bukan inspeksi baca-saja. Tidak menjalankan fixture dan tidak membuat tiket.

Contoh hanya untuk DB disposable/host yang telah disetujui; ganti placeholder resource sebelum approval eksekusi:

```bash
umask 077
# Isi path absolut DB yang telah diverifikasi operator, bukan default tebakan.
read -r -p 'Path absolut database existing: ' bootstrap_db
read -r -p 'Email akun pribadi: ' bootstrap_email
read -r -p 'Nama pengguna: ' bootstrap_name
read -rs -p 'Password baru (14–1024 karakter): ' bootstrap_password
printf '\n'
printf '%s\n' "$bootstrap_password" | node app/bootstrap-admin.js "$bootstrap_db" "$bootstrap_email" "$bootstrap_name"
unset bootstrap_password bootstrap_db bootstrap_email bootstrap_name
```

Password hanya stdin (tidak argv/env/history), output generik tanpa hash/password/token. Jangan jalankan shell tracing/set -x. Login di tujuan HTTPS terverifikasi; alamat HTTP publik port3000 belum memenuhi syarat untuk memasukkan password operasional. Setelah bootstrap berhasil, ADM masuk ke Pengguna; jangan membagikan akun admin. Marker sekali tetap ada walau tabel users diubah operator; tidak ada bootstrap ulang/reset bawaan.

## API dan alur

- GET/POST /api/admin/users: ADM-only list/create. Create {name,email,role,password}, password sementara wajib ganti saat login.
- GET/PATCH /api/admin/users/:id: ADM-only detail/update. PATCH {name,email,role,active:boolean}; semua sesi target dicabut. No hard delete endpoint; deactivate/reactivate mempertahankan foreign key dan history. Jangan reassign ke pengguna nonaktif.
- POST /api/admin/users/:id/reset-password {password}: ADM-only, wajib ganti saat login berikut, seluruh sesi target dicabut.
- POST /api/password/change {current_password,new_password}: akun login sendiri, password baru berbeda14–1024 karakter; limit5/15 menit; selain sesi saat ini dicabut. Akun must_change_password hanya boleh me/logout/change (public routes tetap public).

Responses admin hanya id/name/email/role/active/must_change_password. Password/hash/session tidak dikembalikan. Role enum existing OPR/SUP/L2/L3/CMR/AUD/ADM. ADM metadata user bukan hak baca tiket/audit. User mutations dan audit satu transaksi; last-active-admin dicegah transaksi dan trigger UPDATE/DELETE. Daftar/detail dapat dibaca ADM saja; menebak ID tidak menambah akses. Konflik email/last admin409, input400, akses403, unknown404.

UI mengikuti card/form/brand, native password, daftar kosong, loading/error terarah dan state peran; password/current_password/new_password tidak disalin ke draft RAM atau Web Storage. Nama/email draft masih RAM sesuai lifecycle aplikasi. Native browser/review UX belum dijalankan pada delta ini. Nilai reset ditetapkan operator melalui kanal aman, tidak dikirim via email atau chat oleh aplikasi.

## Migrasi dan rollback

Startup menambah users.name default'', active default1, must_change_password default0; menjaga ID/email/hash/role existing. bootstrap_state marker dan trigger last_admin ditambahkan. Tidak menghapus user/history/tickets; schema user_version historis tetap1, jadi health schema1 tidak membuktikan kolom mana sudah terpasang. Backup konsisten pra-upgrade dan integrity_check wajib sebelum eksekusi pada tujuan yang disetujui. Uji migrasi hanya database sintetis lokal.

Rollback aman bukan langsung menjalankan backend68476bb pada DB yang memiliki pengguna nonaktif/reset: backend lama tidak menegakkan active/must_change_password. Bila terpaksa rollback, hentikan layanan/login, simpan snapshot DB saat ini, restore backup pra-upgrade ke path baru hanya dalam maintenance yang disetujui dengan konsekuensi kehilangan perubahan setelah backup; atau lakukan forward-fix. Jangan menghapus kolom user/approval atau seluruh audit secara manual. Revert kode saja cukup untuk review lokal disposable, tetapi membuka kembali risiko auth jika diakses pengguna. Jangan membuat prosedur rollback live tanpa resource/backup/approval yang tepat.

Gap: checker approval umum perubahan role/pengguna oleh CMR belum tersedia; assignment ini meminta ADM CRUD langsung. PM perlu merekonsiliasi dengan acceptance config maker-checker sebelum klaim seluruh F1/rilis. Tidak ada email reset, MFA, password recovery publik, import akun lama atau keputusan retensi baru. Prod host/DB/SHA/HTTPS/provisioning tetap blocker ACCESS-LOGIN-01, tidak diselesaikan oleh kode ini.
