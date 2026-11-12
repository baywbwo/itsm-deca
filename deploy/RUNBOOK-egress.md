# Runbook: egress proxy (CP6b)

Semua panggilan keluar dari ITSM (Jira Cloud, Lark/Feishu, dan discovery/JWKS/token IdP perusahaan) hanya boleh lewat
egress proxy. Jaringan `backend` tetap `internal: true`: `api`, `postgres`, dan `clamav` tidak punya rute keluar.
Hanya service `egress-proxy` yang juga terhubung ke jaringan `egress`.

```
api ──(backend, internal)──▶ egress-proxy (Squid 5.7) ──(egress)──▶ *.atlassian.net / open.larksuite.com / open.feishu.cn / IdP:443
```

## Aturan proxy (`deploy/egress/squid.conf`)
Urutan aturan; yang pertama cocok menang.

| # | Aturan | Hasil bila dilanggar |
|---|---|---|
| 1 | Sumber harus di `ITSM_EGRESS_CLIENT_CIDRS` (default subnet backend) | 403 |
| 2 | Kredensial proxy (basic auth, file hash `egress_proxy_htpasswd`) | 407 |
| 3 | Hanya metode `CONNECT` (HTTP biasa ditolak) | 403 |
| 4 | Hanya port 443 | 403 |
| 5 | Tujuan berupa alamat IP literal ditolak | 403 |
| 6 | Host harus cocok `deploy/egress/allowlist.regex` | 403 |
| 7 | IP hasil DNS tidak boleh privat/reserved (rentang sama dengan `providers.rs`, termasuk CP5B-07) | 403 |
| 8 | Maksimal 200 koneksi per klien | 403 |

- Tanpa TLS interception (`ssl_bump` tidak dipakai): TLS berjalan end-to-end antara `api` dan penyedia, dan sertifikat asli penyedia diverifikasi oleh `api`.
- Tanpa cache. Resolver DNS proxy: `ITSM_EGRESS_DNS` (default `1.1.1.1 8.8.8.8`), sehingga nama service internal compose tidak bisa di-resolve proxy.
- Log akses: satu baris JSON per request ke stdout (`docker compose logs egress-proxy`). Isinya hanya waktu, IP sumber, metode, host, port, status, hasil, byte, dan durasi. Tidak ada user, header `Authorization`, path, query, atau body.

## Sisi aplikasi
| Env | Nilai |
|---|---|
| `ITSM_EGRESS_PROXY` | `http://host:port`, mis. `http://egress-proxy:3128`. Wajib di production (integrasi selalu `live` di production); tanpa ini API tidak start. |
| `ITSM_EGRESS_PROXY_AUTH_FILE` | File secret berisi satu baris `user:password` (password 16–256 karakter). Wajib di production. File kosong dianggap tidak diisi. |

- Dengan proxy, `api` tidak me-resolve DNS sendiri untuk penyedia. Request menjadi `CONNECT host:443` ke proxy, dan proxy yang memeriksa IP hasil DNS (anti DNS rebinding).
- Aturan aplikasi tetap berlaku sebelum proxy: host Jira/Lark yang diizinkan (CP5B-02), host IdP per perusahaan (issuer + host tambahan), https saja, tanpa redirect, body maksimal 1 MB. Tujuan berupa IP literal ditolak di aplikasi.
- Variabel `HTTP(S)_PROXY` dari environment proses tidak pernah dipakai.
- Saat start, log `outbound calls through the egress proxy` mencatat alamat proxy dan apakah kredensial terpasang (nilainya tidak).
- Tanpa `ITSM_EGRESS_PROXY` (hanya UAT/dev): panggilan langsung dengan DNS di-pin dan IP privat ditolak (perilaku CP5b).
- `ITSM_INTEGRATIONS_LAB=on` / `ITSM_OIDC_LAB=on` (stub lokal, UAT saja) tidak bekerja lewat proxy: stub http/IP privat ditolak proxy dan aplikasi. Lab hanya untuk mode tanpa proxy.

### Error dari proxy
reqwest tidak membuka kode status `CONNECT`. Karena itu semua penolakan tunnel (403 kebijakan, 407 kredensial, 5xx) dicatat sebagai
`Connect` (sementara): UI menampilkan "Penyedia tidak dapat dihubungi", outbox mencoba ulang, dan SSO menjawab `idp_unavailable`.
Log `api` menyebut penyebabnya dengan field `proxy.answer`:
- `refused`: proxy menolak tujuan (host di luar allowlist, port, IP privat setelah DNS) atau upstream gagal;
- `auth_required`: kredensial proxy salah atau tidak cocok dengan `egress_proxy_htpasswd`.

Ini menyimpang dari desain awal (403 → `Refused` permanen). Akibatnya, host yang ditolak proxy akan dicoba ulang outbox sampai batas percobaan.
Penolakan host tetap terjadi di aplikasi lebih dulu (CP5B-02), jadi kasus ini hanya muncul bila allowlist proxy tertinggal dari aplikasi.

## Persiapan (UAT compose)
Buat sepasang kredensial. Password tidak pernah dicetak:
```sh
cd deploy/uat
pw=$(openssl rand -hex 24)
printf 'itsm-api:%s\n' "$pw" > secrets/egress_proxy_auth
printf '%s' "$pw" | openssl passwd -6 -stdin | sed 's/^/itsm-api:/' > secrets/egress_proxy_htpasswd
unset pw
chmod 444 secrets/egress_proxy_auth secrets/egress_proxy_htpasswd
./check-config.sh      # memeriksa format dan bahwa password cocok dengan hash
```
- Rotasi: buat pasangan baru dengan cara yang sama, lalu `docker compose up -d egress-proxy api` (proxy dulu, lalu api).
- `egress_proxy_auth` hanya dipasang ke `api`. `egress_proxy_htpasswd` hanya dipasang ke `egress-proxy`. `postgres` dan `clamav` tidak mendapat keduanya.
- Opsional di `.env`: `ITSM_EGRESS_DNS=<ip> <ip>` untuk resolver lain (mis. resolver kantor).

## Menambah host (mis. IdP perusahaan)
1. Ajukan host dan alasannya untuk security review. Host baru membuka jalur keluar baru.
2. Tambahkan satu baris regex yang di-anchor penuh (`^...$`, titik di-escape) ke `deploy/egress/allowlist.regex`. Contoh IdP ada di file itu sebagai komentar.
3. Commit lewat PR, lalu di host: `docker compose -f deploy/uat/compose.yaml restart egress-proxy` (allowlist di-mount read-only dari repo).
4. Untuk IdP: isi juga issuer dan host tambahan di halaman **Login SSO** (lihat `RUNBOOK-sso.md`). Host tambahan di UI tidak mengubah allowlist proxy; keduanya harus cocok.
5. Uji dengan **Uji koneksi** (Jira/Lark atau SSO), lalu periksa log proxy: baris `"result":"TCP_TUNNEL"` untuk host itu.

### Keputusan: `ITSM_INTEGRATIONS_EXTRA_HOSTS` tidak diimplementasikan (R1)
CP5B-02 mengusulkan env ini untuk host Jira/Lark tambahan. Keputusannya: tidak perlu di R1.
- Jira Cloud REST selalu di `<site>.atlassian.net`. Lark/Feishu memakai dua host tetap.
- Host IdP sudah diatur per perusahaan lewat issuer + host tambahan (pencocokan persis).
- Host baru tetap harus ditambahkan ke allowlist proxy lewat security review. Env kedua di aplikasi hanya menambah satu tempat lagi yang bisa tidak sinkron.
- Bila nanti ada Jira Data Center/self-hosted atau domain kustom, keputusan ini dibuka lagi bersama perubahan allowlist proxy.

## Rollback
| Langkah | Efek |
|---|---|
| `docker compose stop egress-proxy` | Semua panggilan keluar gagal (`Connect`, dicoba ulang outbox). Login SSO menjawab `idp_unavailable`. Login sandi dan fitur lain tetap jalan. |
| Kembalikan `allowlist.regex` ke commit sebelumnya, lalu restart `egress-proxy` | Host baru tertutup lagi. |
| SHA sebelum CP6b | Compose lama tanpa proxy: `backend` internal, tidak ada egress sama sekali (seperti sebelum CP6b). |

Tidak ada migrasi DB di CP6b.

## Pemetaan uji E1–E11 (security review CP5b)
| Uji | Cara | Status |
|---|---|---|
| E1 api tanpa proxy tidak bisa keluar | Statis: `api` hanya di `backend` (`internal: true`) | Statis saja (Docker tidak tersedia di host build) |
| E2 CONNECT host allowlist, TLS end-to-end | Squid nyata: Atlassian, Lark, Feishu 200; sertifikat asli; klien reqwest `api` lewat Squid | Lulus |
| E3 host di luar allowlist | Squid nyata: 6 host → 403 | Lulus |
| E4 port selain 443 | Squid nyata: 8443 dan 80 → 403 | Lulus |
| E5 IP literal | Squid nyata (6 alamat) + aplikasi menolak sebelum request | Lulus |
| E6 host allowlist yang di-resolve ke IP privat | Squid nyata dengan hosts file uji (10/8, 127/8, 169.254, ::1, 6to4) → 403 | Lulus |
| E7 HTTP biasa | Squid nyata → 403, path/query tidak masuk log | Lulus |
| E8 db/clamav ke proxy | Squid nyata: tanpa/salah kredensial 407, sumber di luar CIDR 403; statis: db/clamav tanpa secret proxy | Lulus (runtime antar-container belum) |
| E9 production tanpa proxy | Tes config: gagal start tanpa `ITSM_EGRESS_PROXY` atau tanpa kredensial | Lulus |
| E10 Jira dan Lark nyata lewat proxy + outbox | Butuh token nyata | TUNGGU owner |
| E11 log tanpa secret | Squid nyata: JSON, allow/deny tercatat, tanpa password/user/Authorization/path/query | Lulus |

Bukti: `evidence/i3/cp6b/` di repo dokumentasi (harness Squid, cek statis compose, cek `check-config.sh`).
