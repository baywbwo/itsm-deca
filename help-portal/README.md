# Portal pengaduan DECA

Jalankan server.mjs dengan Node.js 24 di host portal. Arahkan GET /help/ dan POST /help/api/itsm-tickets pada reverse proxy portal ke http://127.0.0.1:3100 dengan path yang sama. Pasang batas laju per IP pada POST di proxy yang menerima koneksi konsumen. Gunakan HTTPS untuk portal publik.

Service membaca ITSM_USERNAME dan ITSM_PASSWORD dari environment proses. Gunakan akun intake yang didukung kebijakan autentikasi ITSM dan memiliki izin ticket.create pada workspace DECA. Login yang memerlukan interaksi MFA tidak dapat digunakan oleh adapter ini. ITSM_WORKSPACE_ID berisi UUID workspace dan ITSM_CHANNEL_ID berisi UUID kanal intake aktif milik workspace itu. ITSM_ORIGIN secara default menggunakan https://itsm.scowork.my.id:8443. PORT secara default 3100.

Setelah environment tersedia, jalankan:

    node server.mjs

Portal meneruskan laporan ke POST /api/v1/workspaces/{workspace_id}/tickets. Kategori, nama merchant, PIC, kontak dan kendala masuk ke detail yang dibaca CS. ITSM memberikan nomor tiket. Penugasan awal mengikuti aturan workspace. CS menangani tiket dan memilih tindak lanjut Jira atau Lark di ITSM.

Verifikasi deployment dengan laporan uji di workspace uji. Cocokkan nomor yang ditampilkan portal dengan nomor pada daftar tiket baru dan periksa detail kontak serta kronologinya.

## Email pelanggan

Gunakan Node.js 24 dan Python 3. Worker menyimpan penerima dan antrean di SQLite (PORTAL_DB; default notifications.sqlite di direktori service). Jalankan satu instance untuk setiap database antrean. Worker membaca riwayat status ITSM setiap 30 detik, termasuk perubahan yang terjadi saat portal offline. Tanda terima dan perubahan status dikirim berurutan per tiket; kegagalan SMTP dicoba kembali. SMTP yang menerima pesan sesaat sebelum proses berhenti bisa menerima ulang pesan dengan Message-ID yang sama.

Atur SMTP_HOST, SMTP_PORT, SMTP_FROM, SMTP_USERNAME dan SMTP_PASSWORD_FILE di environment privat service. File password hanya perlu dapat dibaca pemilik service. Port 465 memakai TLS langsung dan port lain memerlukan STARTTLS. Untuk Gmail gunakan smtp.gmail.com, port 465, alamat Gmail sebagai SMTP_USERNAME dan SMTP_FROM, serta App Password dari akun itu. Sender resmi DECA dapat digunakan setelah relay mengizinkan alamat cs@deca.co.id. SMTP dan konfigurasi ITSM tetap berada di server.

Migrasi 0033 dan API/UI terkait menambahkan kontrol jeda dan lanjutkan pada menu Integrasi per workspace. Email tetap diantrekan saat dijeda. Instalasi lokal yang masih memakai API ITSM lama dapat memakai EMAIL_LEGACY_API=1 untuk mengirim ketika endpoint pengaturan belum tersedia. Setelah API baru dipasang, hapus opsi transisi itu.
