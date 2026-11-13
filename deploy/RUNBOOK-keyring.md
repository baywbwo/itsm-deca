# Runbook: keyring dan rotasi kunci (CP5B-03, CP5B-04)

Keyring mengenkripsi kredensial Jira/Lark dan seed TOTP (AES-256-GCM). Isinya
`key_id:base64(32 byte)[,key_id:...]`; `ITSM_KEYRING_ACTIVE` menentukan kunci untuk enkripsi baru.

## Tempat kunci (CP5B-04)
- Kunci hanya ada di file secret, dibaca server lewat `ITSM_KEYRING_FILE`. Di UAT compose path-nya
  `/run/secrets/itsm_keyring`, dari `deploy/uat/secrets/itsm_keyring`.
- Production menolak start bila `ITSM_KEYRING` (atau `ITSM_MFA_RECOVERY_PEPPER`) diisi sebagai env var.
  Pakai `ITSM_KEYRING_FILE` / `ITSM_MFA_RECOVERY_PEPPER_FILE`. Env var terlihat lewat `docker inspect` dan
  `/proc/<pid>/environ`.
- Mengisi env dan `_FILE` sekaligus ditolak. File kosong berarti tidak ada keyring.
- Syarat file: regular file, maksimal 64 KB, tidak writable oleh user lain. Server membacanya sekali saat
  start, lalu buffer dihapus dari memori.
- UAT compose non-swarm memasang file secret dengan izin host. Pakai `chmod 444` di dalam direktori
  `secrets/` ber-`chmod 700`; `check-config.sh` memeriksa keduanya.
- Membuat kunci: `printf 'k2026a:%s' "$(openssl rand -base64 32)"`. Jangan dicetak ke chat, tiket, atau log.

## Rotasi (pensiunkan kunci lama atau yang bocor)
Contoh: kunci lama `k2025`, kunci baru `k2026a`. Jalankan dari `deploy/uat`.

1. **Tambah kunci baru, kunci lama tetap ada.**
   Isi `secrets/itsm_keyring` = `k2025:<lama>,k2026a:<baru>`.
2. **Aktifkan kunci baru.** Di `.env`, set `ITSM_KEYRING_ACTIVE=k2026a`, lalu jalankan `./check-config.sh` sampai OK.
3. **Restart api.** `docker compose up -d api`. Kredensial baru sekarang dienkripsi dengan `k2026a`.
4. **Uji dulu (dry run).** `docker compose run --rm api reseal --dry-run`
   - Hasilnya `reseal --dry-run: resealed=N failed=F remaining=R`, tanpa menulis ke DB.
   - Bila `failed > 0`, exit code bukan 0. Log `warn` mencatat `secret_id` dan `key_id` baris yang tidak bisa dibuka; isi secret tidak pernah dicatat.
   - Baris seperti itu dienkripsi dengan kunci yang tidak ada di keyring, atau datanya rusak. Pemilik kredensial harus memasukkan ulang kredensialnya lewat UI Integrasi, karena baris itu tidak bisa di-reseal.
5. **Re-enkripsi.** `docker compose run --rm api reseal`
   - Baris diproses per batch 500 dalam urutan id. Baris yang gagal dilewati, jadi tidak menghentikan baris lain.
   - Exit code 0 hanya bila `remaining=0`, artinya tidak ada lagi baris dengan kunci non-aktif.
   - Tiap baris yang di-reseal mendapat `version + 1` dan `rotated_at = now()`.
6. **Verifikasi.** Ulangi langkah 5 sampai keluar `reseal: resealed=0 failed=0 remaining=0`, dengan exit code 0.
7. **Hapus kunci lama.** Isi `secrets/itsm_keyring` = `k2026a:<baru>` saja, lalu jalankan `./check-config.sh` dan `docker compose up -d api`.
   Cek halaman Integrasi: status kredensial "Tersimpan" dan Test koneksi berhasil. Untuk TOTP, cek login MFA.
8. **Simpan catatan rotasi.** Catat tanggal, key_id lama dan baru, dan output langkah 5–6 (hanya angka) di STATUS/tiket change.
   Kunci lama dimusnahkan dari semua salinan, termasuk backup `.env` lama.

Rollback sebelum langkah 7: kembalikan `ITSM_KEYRING_ACTIVE` ke kunci lama, lalu restart.
Kedua kunci masih ada, jadi semua baris tetap bisa dibuka.
Setelah langkah 7, rollback hanya bisa dilakukan bila kunci lama masih disimpan di tempat aman.

## Batas yang diketahui
- `reseal` tidak menulis event audit ke DB, karena tabel audit platform butuh aktor pengguna.
  Jejaknya ada di `version`/`rotated_at` per baris, di output perintah, dan di catatan change.
- Backup DB lama tetap terenkripsi dengan kunci lama. Pertimbangkan masa retensinya sebelum memusnahkan kunci.
