# DECA ITSM — Support Desk

Aplikasi tiket layanan pelanggan & merchant DECA (engine Zammad 7.2.2).

| Path | Fungsi |
|---|---|
| `index.html` | SPA support desk: login Zammad, filter status + badge, inbox, detail tiket, balas/catatan internal, ubah status, buat tiket, healthcheck, responsif mobile |
| `api/help_api.py` | Bridge: tiket Zammad + Base Sales (`Ticket`) + Base CS (`Laporan Penanganan Pengaduan`, 17 kolom PADG 32/2026) |
| `api/cif.py` | Adapter CIF (fail-open, cache 5 menit, env `CIF_BASE_URL`/`CIF_TOKEN`) |
| `deploy/deploy.sh` | Deploy Zammad + UI + service bridge |

## 11 status pengaduan (PADG 32/2026)
Informasi · Transaksi Berhasil Diselesaikan · Pengembalian Dana Penuh · Pengembalian Dana Sebagian · Penghapusan/Pengembalian Biaya · Akses atau Akun Dipulihkan · Klaim Konsumen Ditolak · Dieskalasikan · Mediasi/Sengketa · Dalam Investigasi · Belum terselesaikan

## Env
`LARK_TOKEN` `ZAMMAD_URL` `ZAMMAD_USER` `ZAMMAD_PASS` `CS_APP` `CS_LAPORAN` `SALES_APP` `SALES_TICKET`

> Portal publik `/help` tidak ada di repo ini (ditempel di repo compro).
