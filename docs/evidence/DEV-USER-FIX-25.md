# DEV-USER-FIX-25 — exact bootstrap path dan recovery pengguna

Tanggal 2026-10-30; owner Developer; reviewer PM dan QA independen berikutnya. Basis ac463de1bc5dde6a5185f0b5d97946b819a85b17, branch pilot/itsm-revamp-v2. Requirement USER01/06/08 dan defect QA24-D01 Medium ditetapkan PM; sumber kontrak docs/USERS.md, DEV-USER-23 dan /data/projects/itsm/preparation-v2/05-qa/24-user-management.md. Ini bukti Developer, bukan closure QA atau seluruh USER/F1.

## Perubahan dan hasil aktual

- CLI memakai guard sebelum probe read-only: lstat seluruh parent/file, reject symlink dan noncanonical path, file harus existing regular file; descriptor O_RDONLY|O_NOFOLLOW ditahan. Device/inode descriptor dan seluruh komponen dibandingkan lagi setelah stdin sebelum createDomain, lalu sebelum bootstrap ADM/marker.
- Test file symlink, parent symlink, missing, directory dan `..` semuanya exit1; byte DB target tetap sama. users dan bootstrap_state tetap kosong. Path normal yang sama exit0. No fixture/ticket dibuat.
- Guard test mengganti file dengan inode lain, mengganti dengan symlink, dan mengganti parent directory: ketiganya ditolak saat recheck; byte resource awal tidak berubah. Ini penggantian deterministik antar checkpoint, bukan stress race filesystem.
- Suite fault atomic user/bootstrap/audit existing dijalankan bersama regresi, tanpa mengurangi assertion.

Node v24.22.1. `node --test app/test/*.test.js` dari checkout: **46 PASS, 0 FAIL, 0 SKIP**. Archive staged tree lengkap dibuat dengan `git archive $(git write-tree) | tar -x -C <disposable>`; perintah suite yang sama dari archive tanpa .git: **46 PASS, 0 FAIL, 0 SKIP**. Log lokal /tmp/dev-user25-suite.log dan /tmp/dev-user25-archive.log; runner/tes committed agar QA dapat mereproduksi dari SHA stabil.

SHA256:
- app/bootstrap-path.js: 4879147c9cb007c70fccb2637fd5f5ddf0e83de0a877cc44803e503d147e1911
- app/test/bootstrap-path.test.js: c88f1eef0e9dd1da9d67cdce6e2671152c9af433116110ec761e8c8a272dc64f
- docs/evidence/runners/user25-recovery.mjs: 8cd78305545f5bfa71288b9e8580811427174843d33e9be316523fbc62fe9ab4

## Recovery aktual dan reproduksi

Buat baseline lengkap: `git archive 68476bbbcc5a717e6da7d88eb4a2117858b9fdec | tar -x -C <old-archive>`. Jalankan `node docs/evidence/runners/user25-recovery.mjs /absolute/old-archive` dari checkout/archive baru. Runner tidak membutuhkan .git, baseline disediakan eksplisit, semua DB/data sintetis dalam tmp disposable dan dibersihkan.

**PASS**: source lama membuat ADM+dua OPR; VACUUM INTO backup sebelum upgrade. Source baru create akun tambahan, deactivate OPR, reset OPR lain: login nonaktif/password lama401, password reset200 dengan force-change1. Stop listener+close DB lalu request login gagal koneksi. Restore backup ke path baru (simpan DB post-upgrade asli sampai akhir run), source lama: semua row user sama dengan pra-backup, password asli kembali200 dan akun baru401; integrity ok. Stop/login closed kembali; reupgrade source baru mempertahankan id/email/hash/role, active1/force-change0 sesuai baseline, marker kosong; login OPR200; restart integrity ok, jumlah user3. Output runner PASS. Tidak menjalankan backend lama pada DB post-upgrade yang masih berisi pembatasan akses baru.

Rollback wajib stop/login closed, snapshot konsisten post-upgrade untuk investigasi, restore backup pra-upgrade+source lama dalam maintenance ke path fresh, integrity dan review akses sebelum buka kembali. **Write setelah backup hilang**, termasuk create/deactivate/reset/sesi/audit; status nonaktif dan password reset tidak dipertahankan, password lama kembali berlaku. Source downgrade saja tidak aman. Forward-fix lebih baik bila kehilangan write tidak diterima. Guard-only revert tidak membutuhkan penghapusan schema tetapi mengembalikan defect symlink; jangan gunakan bootstrap lama pada alias.

## Risiko dan gap eksplisit

Node SQLite membuka pathname sendiri; ini bukan openat traversal/binding atomik. Tidak menjamin bebas TOCTOU antara check/open/chmod/schema ataupun sidecar. Operator harus menguasai file/parent/sidecar dan mencegah penulis filesystem bersamaan; tanpa itu mutasi target yang ditukar tepat saat open masih mungkin. Tidak mengklaim filesystem sandbox atau race-proof. Kebijakan guard terbatas CLI bootstrap.

Maker-checker perubahan user/role **OPEN**: ADM masih bisa CRUD/reset tanpa checker CMR; audit setelah aksi tidak menggantikan approval. Browser-native/keyboard/responsif **NOT RUN**. QA retest independen masih diperlukan; tidak menyatakan seluruh USER01..08 atau F1 lulus. ACCESS-LOGIN-01 terpisah dan tidak dikerjakan ulang. Tidak live/provision3000/push/merge/deploy atau modifikasi untracked. PR #1 tetap DRAFT; commit lokal ini menunggu review delta PM.
