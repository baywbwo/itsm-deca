# Review lokal F1 — DEV-F1-09

Baseline 2a68ccb, branch pilot/itsm-revamp-v2. Runtime Node 24.22.1. Gunakan SHA stabil pada handoff; jangan menguji working tree yang sedang ditulis. `npm test` menjalankan HTTP/SQLite nyata; bukti developer ada di docs/evidence/developer-product-tests.tap. Ini bukan review QA independen.

## URL dan keterjangkauan

URL layanan fixture: http://127.0.0.1:3100/ ; intake: http://127.0.0.1:3100/#intake ; /healthz dan /version.
Pemeriksaan HTTP lokal menghasilkan 200 pada ketiganya. Browser tab di Screen panel gagal membuka URL dengan ERR_SOCKS_CONNECTION_FAILED dan penolakan loopback oleh permukaan browser. Karena itu keterjangkauan browser dan seluruh review visual **belum terverifikasi**. Tidak membuka port publik atau mengubah routing untuk melewati pembatasan. PM perlu menyediakan pemetaan fixture yang didukung Browser tab untuk UX.

Proses review sementara pada port 3100 memakai app/data/review.sqlite (disposable, di-ignore); seluruh akun berpassword acak yang tidak dicetak/disimpan terpisah. Ini tidak menyediakan login siap pakai kepada reviewer. Untuk review dengan login, hentikan proses fixture tersebut dan provision database baru dengan password pilihan reviewer melalui input tersembunyi berikut. Jangan memakai database existing: seed tidak mengganti password akun yang sudah ada.

```bash
# Dari checkout SHA stabil; port 3100 harus bebas.
umask 077
read -rs -p 'Password fixture sementara: ' FIXTURE_PASSWORD
export FIXTURE_PASSWORD
# Pilih path baru yang belum ada, hanya untuk fixture disposable.
export APP_DB="$(mktemp -d /tmp/dob-review.XXXXXX)/fixture.sqlite"
NODE_ENV=development APP_FIXTURES=1 npm run fixture
unset FIXTURE_PASSWORD
NODE_ENV=development APP_FIXTURES=1 APP_COMMIT="$(git rev-parse HEAD)" npm start
```

Login peran melalui <role>@example.test: opr, sup, l2, l3, cmr, aud, adm. Password hanya yang dimasukkan reviewer, tidak dikirim ke chat. Seed tiga tiket is_test dan dua artikel/macro. Semua request intake pada mode fixture juga is_test. Ekspor default akan kosong untuk data ini (kontrol pemisahan); uji ekspor nonkosong ada di database in-memory disposable pada npm test. Jangan menonaktifkan pengecualian is_test di layanan ini.

## Skenario dan batas

OPR: intake → polling → triase complaint → klaim → Diproses → balasan/internal → Menunggu Pelapor → Teratasi dengan status pengaduan → ajukan laporan. CMR: review → setujui dengan catatan. OPR ubah severity/status sumber: laporan kembali DATA_DRAFT, checker/note dihapus, version bertambah; persetujuan versi lama 409. Perubahan assignment/eskalasi juga konservatif membatalkan approval. Penghapusan flag complaint setelah review tetap ditolak. Pembatalan approval tercatat audit tanpa menyalin isi pribadi. Auto-close juga membatalkan approval dan membuat outbox sekali secara atomik.

Untuk fake clock/failure gunakan createApp/createDomain di salinan lokal seperti app/test/product.test.js; tidak ada endpoint manipulasi waktu/audit pada layanan. Perintah npm test mencakup kegagalan INSERT audit yang membatalkan perubahan tiket dan auto-close.

SLA fixture: Senin–Jumat 08–17 WIB; parameter config/taxonomy.json adalah asumsi. Lampiran 5 MB, 5 berkas/tiket, png/jpg/pdf/txt/csv, belum dipindai malware. Adapter simulasi lokal, bukan pengiriman email atau integrasi live. Daftar lengkap gap terdapat di 03-architecture/09-status-implementasi-f1.md pada preparation-v2.

Kontrak deploy/restore/rollback: docs/DEPLOY.md. Draf PR exact: docs/PR_BODY.md. Tidak push sampai QA/UX terintegrasi PM; push dapat auto-deploy. Jangan menjalankan deploy/deploy.sh.
