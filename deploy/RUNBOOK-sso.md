# Runbook: SSO perusahaan (OIDC, CP5c)

ITSM masuk lewat penyedia identitas (IdP) masing-masing perusahaan dengan OIDC Authorization Code + PKCE (S256).
Setiap perusahaan punya satu IdP. Akun dikenali dari pasangan (IdP, `sub`), bukan dari email.

## Syarat server
| Env | Nilai | Catatan |
|---|---|---|
| `ITSM_ACCESS_MODE` | `password` | SSO tidak berjalan di `uat_superadmin`. Compose UAT di repo ini memakai `uat_superadmin`, jadi `check-config.sh` menolak `ITSM_AUTH=sso\|both` di sana. |
| `ITSM_AUTH` | `password` (default), `both`, atau `sso` | `both`: SSO dan sandi. `sso`: hanya SSO; sandi tinggal untuk admin platform (break-glass). |
| `ITSM_PUBLIC_URL` | origin https, mis. `https://itsm.example.co.id` | Wajib untuk `both`/`sso`. Redirect URI = `<origin>/api/v1/auth/oidc/callback`. Nilai persisnya juga tampil di halaman **Login SSO**. |
| `ITSM_KEYRING_FILE` + `ITSM_KEYRING_ACTIVE` | lihat `RUNBOOK-keyring.md` | Client secret disegel keyring (purpose `oidc_client_secret`). Tanpa keyring SSO tidak bisa diaktifkan. |
| `ITSM_OIDC_LAB` | `off` | `on` hanya untuk stub IdP lokal di UAT; production menolak start. |

- Server harus bisa menjangkau host IdP lewat HTTPS 443. Discovery, JWKS, dan token endpoint memakai guard egress yang sama dengan Jira/Lark: tanpa redirect, body maksimal 1 MB, IP literal dan IP privat ditolak.
- Di production lalu lintas keluar wajib lewat egress proxy (CP6b, `ITSM_EGRESS_PROXY`). Tambahkan host IdP di bawah ini ke `deploy/egress/allowlist.regex` lewat security review (lihat `RUNBOOK-egress.md`). Host tambahan di halaman **Login SSO** tidak mengubah allowlist proxy; keduanya harus cocok.
- Host yang boleh dihubungi untuk satu IdP: host issuer, ditambah **host tambahan** yang diisi admin, dengan pencocokan persis.

## Mendaftarkan aplikasi di IdP
Isi di sisi IdP selalu sama:
- Tipe: aplikasi web *confidential* (punya client secret).
- Grant: authorization code.
- Redirect URI: persis `<ITSM_PUBLIC_URL>/api/v1/auth/oidc/callback`.
- Scope: `openid email profile`.

Client secret hanya dimasukkan lewat form "Client secret" di halaman **Login SSO**. Jangan kirim lewat chat, tiket, atau email.

### Microsoft Entra ID
1. Buka Entra admin center → App registrations → New registration. Pilih *Accounts in this organizational directory only*. Isi redirect URI tipe **Web**.
2. Buka Certificates & secrets → New client secret. Catat masa berlakunya: secret yang kedaluwarsa memutus login SSO.
3. Peran, pilih salah satu:
   - **App roles** (disarankan): buat role, mis. `itsm-cs` dan `itsm-lead`. Tetapkan ke user atau grup di Enterprise applications. Di ITSM, isi nama klaim grup `roles`.
   - **Groups claim**: Token configuration → Add groups claim → Security groups. Nilainya object ID (GUID) grup. Bila user punya lebih dari 200 grup, Entra tidak mengirim klaim ini (overage) dan user tidak mendapat peran.
4. Isian ITSM:
   - Issuer `https://login.microsoftonline.com/<tenant-id>/v2.0`.
   - Host tambahan: kosong. Semua endpoint ada di `login.microsoftonline.com`.
5. Entra tidak mengirim `email_verified`. Karena itu ITSM **tidak** membuat akun otomatis untuk user Entra. Admin menautkan akun yang sudah ada lewat **Kelola → Akun terhubung → Hubungkan akun**, dengan mengisi "Subject di penyedia" (`sub` user).
   - `sub` Entra berbeda per aplikasi dan tidak tampil di portal. Lihat Gap.

### Google Workspace
1. Buka Google Cloud console → APIs & Services → Credentials → Create OAuth client ID → Web application. Isi Authorized redirect URI.
2. Atur OAuth consent screen ke **Internal** supaya hanya akun domain Workspace yang bisa masuk.
3. Isian ITSM:
   - Issuer `https://accounts.google.com`.
   - Host tambahan: `oauth2.googleapis.com` (token endpoint) dan `www.googleapis.com` (JWKS).
   - Domain email: domain Workspace perusahaan.
4. Google tidak mengirim klaim grup. Akun yang dibuat otomatis tidak mendapat peran dan melihat layar "Menunggu akses" sampai admin memberi peran di **Pengguna**.

### Keycloak
1. Di realm perusahaan, buat Client → OpenID Connect:
   - Client authentication **On**.
   - Standard flow **On**.
   - Direct access grants Off.
   - Valid redirect URIs: persis redirect URI di atas.
   - Advanced → PKCE method `S256`.
2. Buat mapper grup: Client scopes → `<client>-dedicated` → Add mapper → Group Membership.
   - Token claim name `groups`.
   - **Full group path Off**.
   - Add to ID token On.
3. Isian ITSM:
   - Issuer `https://<host>/realms/<realm>`.
   - Host tambahan: kosong, kecuali *Frontend URL* realm memakai host lain.
4. Keycloak mengirim `email_verified`. Pastikan verifikasi email realm aktif bila memakai pembuatan akun otomatis.

## Mengaktifkan untuk satu perusahaan
Lakukan di menu Administrasi → **Login SSO** sebagai admin platform. Dengan `ITSM_MFA=on`, setiap perubahan butuh step-up.

1. Klik **Atur**. Isi:
   - nama tampilan, issuer, dan client ID;
   - domain email untuk pembuatan akun otomatis (kosong = tidak ada akun otomatis);
   - host tambahan;
   - nama klaim grup dan pemetaan grup → peran (`cs` / `lead_cs`; `lead_cs` menang bila keduanya cocok);
   - opsi **Perbarui peran dari grup di setiap login**. Bila aktif, peran disesuaikan pada setiap login. Bila tidak aktif, peran hanya diberikan saat akun dibuat.
2. Simpan client secret.
3. Klik **Uji koneksi**.
   - Hasil gagal menyebut tahapnya: `policy` (alamat ditolak), `discovery`, atau `jwks`.
   - Teks dari IdP tidak ditampilkan.
4. Klik **Aktifkan SSO**. Pengguna masuk di halaman login dengan **Kode perusahaan** lalu **Masuk dengan SSO**.

Hal yang mematikan atau memutus SSO:
- Mengganti issuer, client ID, atau host tambahan mematikan SSO perusahaan itu dan menghapus hasil uji, jadi uji lagi lalu aktifkan lagi.
- **Matikan SSO** langsung mengakhiri semua sesi SSO perusahaan itu.

## Mode `sso` dan akun break-glass
- Mulai dengan `ITSM_AUTH=both`. Pindah ke `sso` setelah setiap perusahaan aktif dan login SSO-nya terbukti.
- Di mode `sso`, sandi hanya berlaku untuk akun dengan grant `platform_admin`.
  - Akun lain mendapat 401 yang sama dengan sandi salah. Auth event-nya `login denied sso_only`.
  - Sesi sandi akun lain yang masih ada berhenti berlaku.
  - Halaman login menampilkan SSO lebih dulu. Form sandi dilipat dengan label "khusus admin platform".
- Akun break-glass:
  - Minimal dua admin platform, masing-masing dengan sandi kuat dan TOTP (`ITSM_MFA=on`).
  - Kredensial disimpan offline oleh pemilik.
  - Uji login setiap kuartal.
  - Setiap pemakaian dicatat di auth event.
- **Step-up.** Semua aksi yang butuh step-up (buat/aktifkan perusahaan, perubahan SSO) hanya untuk admin platform.
  - Step-up memakai TOTP akun lokal (`POST /session/step-up`).
  - Sesi SSO milik admin platform tanpa sandi dan TOTP lokal tidak bisa step-up. Untuk aksi itu, admin memakai sesi sandi break-glass.
  - Re-autentikasi ke IdP (`prompt=login`/`max_age`) belum dipakai di R1.

## Rollback
| Langkah | Efek |
|---|---|
| Matikan SSO satu perusahaan di halaman **Login SSO** | Login SSO perusahaan itu ditolak (`?sso=not_configured`) dan sesinya berakhir. Data IdP dan tautan akun tetap ada. |
| `ITSM_AUTH=both` lalu restart | Login sandi terbuka lagi untuk semua akun yang punya sandi. |
| `ITSM_AUTH=password` lalu restart | Rute OIDC 404 dan semua sesi SSO berhenti berlaku. Data tetap ada; mengembalikan `ITSM_AUTH` memulihkan perilaku sebelumnya. |
| Migrasi `0026` down | Ditolak selama ada data SSO. Hapus IdP lewat DB hanya dengan keputusan owner. |

## Gangguan umum (`?sso=` di halaman login)
| Kode | Arti | Tindakan |
|---|---|---|
| `not_configured` | Kode perusahaan salah, atau SSO perusahaan belum aktif | Periksa kode dan status di **Login SSO** |
| `unavailable` | Server tanpa keyring (client secret tidak bisa dibuka) | Pasang keyring (lihat `RUNBOOK-keyring.md`), lalu restart |
| `idp_unavailable` | Discovery, JWKS, atau token endpoint tidak terjangkau | Jalankan **Uji koneksi**; periksa allowlist egress proxy dan host tambahan |
| `idp_error` | IdP menolak atau user membatalkan | Lihat log sign-in di IdP |
| `denied` | Token tidak valid (tanda tangan, aud, nonce, atau kedaluwarsa) | Periksa client ID dan jam server (skew maksimal 60 detik) |
| `expired` | Proses lebih dari 10 menit, dipakai ulang, atau dibuka di browser lain | Ulangi dari halaman login |
| `not_linked` | Belum tertaut dan tidak memenuhi syarat akun otomatis | Tautkan akun, atau tambahkan domain email |
| `email_in_use` | Email sudah dipakai akun lain; akun itu tidak diambil alih | Tautkan akun tersebut ke `sub`-nya |
| `inactive` | Akun nonaktif | Aktifkan di **Pengguna** bila memang berhak |

## Gap yang diketahui
- **`sub` Entra untuk penautan.** `sub` Entra berbeda per aplikasi dan tidak terlihat di portal Entra, sehingga admin sulit mendapatkannya. Usulan lanjutan:
  - simpan percobaan `not_linked` (sub, email, nama) dengan masa simpan pendek supaya admin bisa menautkan dari daftar; atau
  - terima klaim opsional `xms_edov` Entra sebagai bukti domain terverifikasi.
  - Keduanya belum diimplementasikan dan perlu diuji di tenant nyata (TUNGGU owner).
- **Rotasi kunci IdP.** Kunci IdP yang berganti dalam kurang dari 60 detik setelah refresh JWKS terakhir ditolak sampai refresh berikutnya.
- **Deprovisioning.** Deprovisioning masih manual: nonaktifkan akun di **Pengguna**. Logout IdP dan SCIM di luar R1.
- **Uji IdP nyata.** Uji ke IdP nyata tiap perusahaan TUNGGU owner, karena butuh tenant dan client per perusahaan.
