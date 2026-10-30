# DEV-USER-23 — acceptance sebelum implementasi
Owner Developer; reviewer QA/PM. Input PM assignment3f3bc622, ACCESS-LOGIN-01; baseline68476bb. Scope lokal, tidak memprovision instance3000.

- USER01 bootstrap CLI: DB absolute existing wajib, STDIN/password tersembunyi, email/nama valid dan password14–1024; sekali saja, fail jika admin/email/marker ada; tanpa fixture/tiket/password default/endpoint publik.
- USER02 ADM-only list/detail/create/update nama/email/role via /api/admin/users; role enum existing, default deny termasuk manipulasi ID; tidak mengembalikan hash/secret.
- USER03 deactivate/reactivate tanpa delete; foreign-key/history/audit terjaga, akun nonaktif tidak bisa login/sesi; perubahan akun mencabut semua sesi target.
- USER04 reset password oleh ADM membuat must_change_password, revoke sesi target. Login berikut hanya boleh me/logout/password change, semua operasi lainnya403; password baru harus berbeda, perubahan selesai atomik dan sesi lain dicabut.
- USER05 admin aktif terakhir tidak dapat dinonaktifkan/didemote/dihapus; transaksi BEGIN IMMEDIATE dan trigger SQLite menjaga invariant lintas koneksi/race.
- USER06 semua mutasi account/password/bootstrap bersama audit atomik; failure tidak menyisakan user/session/status berubah. Audit metadata hanya role/active/ID/reason tanpa password/hash/email/nama lengkap.
- USER07 UI ADM-only daftar/detail/form create/update/deactivate/reactivate/reset serta force change, loading/error/empty sesuai brand. Password tidak masuk draft RAM/WebStorage/log; reset tidak dikirim email/ke chat.
- USER08 tes disposable empty existing DB→CLI→login→kelola, role/IDOR/revoke/concurrent admin removals/fault serta migration/rollback; tidak menjalankan live. Tidak mengklaim owner sudah bisa login3000.

Flow: operator menginisialisasi DB aplikasi lokal, menjalankan bootstrap CLI sekali dengan stdin → ADM login → Pengguna → create/update/status/reset; target login reset→ganti password→workspace. Hard delete tidak tersedia (deactivation sebagai delete aman). Role/config maker-checker approval umum masih gap, bukan ditutup oleh ADM CRUD ini; perlu keputusan PM bila gate lama mewajibkan CMR approval role.

## Implementasi dan evidence

CLI app/bootstrap-admin.js (stdin, exact existing initializedDB, fail-closed dan marker sekali); app/users.js service metadata/CRUD/deactivate/reset/change dengan transaction+audit; domain migrasi additive dan trigger last active ADM; server login/session active checks, force password gate dan admin-only routes; UI Pengguna dan force password page mengikuti brand. Password fields dikecualikan dari semua snapshot draft, termasuk saat error. User list assignment/escalation menyaring akun aktif. Tidak ada delete user API; deactivate adalah operasi hapus logis.

Perintah: npm test > docs/evidence/dev-user-23.tap 2>&1; node --test app/test/ui.test.js app/test/users.test.js > docs/evidence/dev-user-23-targeted.tap 2>&1; node --check fileJS baru/berubah; git diff --check; legacy diff37a9bba. Hasil final suite dicatat pada TAP. Rerun penuh setelah perubahan relevan untuk regresi auth/domain/legacy UI; bukan memakai hasil QA68476bb sebagai hasil kode baru.

Tes: disposable initialized emptyDB→CLI→login→CRUD; missing path tidak membuat DB; bootstrap kedua ditolak;6 role nonADM×5 akses metadata/mutasi ditolak, anonymous401; no hash responses, deactivation/reactivation dan session revoke; reset/password-change gated, password lama ditolak, unchanged password400; parallel HTTP demote2ADM→200/409 dan satu ADM tersisa; direct DB DELETE last admin diblok; fault bootstrap/create/update/reset/change membandingkan user/session/audit snapshot; migration4kolom menjaga ID/hash dan defaults. UI DOM double empty ADM page/denied nonADM/password error+snapshot. Bukan seluruh route×method×role, bukan multi-process stress atau browser visual.

Prosedur operator, migration, deployment/security/rollback: docs/USERS.md; docs/DEPLOY.md tetap runtime dasar. Kode lama tidak menghormati active/must_change_password: jangan rollback live dengan kode lama pada DB baru sambil login dibuka. Stop/backup/maintenance dan restore snapshot yang disetujui atau forward fix; tidak menghapus audit/akun manual. Tidak ada perubahan live/source artifact QA atau public network. ACCESS-LOGIN-01 dan DEV-BROWSER-19 untracked tidak diubah/stage. Unknown prod host/APP_DB/HTTPS/SHA tetap blocker provisioning; owner belum bisa diklaim login.

F1 gap lain tetap, khususnya approval CMR untuk role/config belum diimplementasikan oleh ADM CRUD langsung sesuai assignment; perlu rekonsiliasi PM. Browser-native password/autofill/form/logout/force-change tetap NOT RUN; QA independen SHA baru diperlukan. Tidak push/merge/deploy.

Hasil final aktual: Node24.22.1, **44/44 PASS**,0 FAIL/skip, exit0 pada npm test. Targeted sebelumnya13/13 PASS. Empat fault audit tambahan melalui HTTP create/update/reset/change semuanya500, snapshot users/sessions(identity+expiry)/audit tetap; last_seen middleware dikecualikan karena touch sesi bukan mutasi account. Bootstrap/domain fault juga diuji langsung. Sintaks JS baru/berubah dan whitespace PASS; legacy diff kosong. Tidak ada secret production dipakai. QA belum memverifikasi SHA baru; browser native NOT RUN.

SHA256:
- aac8cb93c71d05c7bb0db772c955a182c278c6a8253ea8494636b012d90c58fa app/users.js
- 28e50f70a9cfd20ada7d377f59e8afe6168375b9667b354f64ad633444c7a6e1 app/bootstrap-admin.js
- 9e5fb4ca082bfbd620d60585a648c1fdad3831cfc1ea94e6418f90ab7663e150 app/test/users.test.js
- b5a311e40f81d3327f106807421932f40e104c4bf8b4e828ebf69df349fab9e2 docs/evidence/dev-user-23.tap
