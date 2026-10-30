import {createDomain,id} from './domain.js';
if(process.env.APP_FIXTURES!=='1'||process.env.NODE_ENV==='production')throw new Error('Fixture memerlukan APP_FIXTURES=1 dan lingkungan non-production');
const pw=process.env.FIXTURE_PASSWORD;if(!pw||pw.length<14)throw new Error('FIXTURE_PASSWORD minimal 14 karakter, masukkan lewat environment lokal; jangan commit');
const d=createDomain(process.env.APP_DB||'app/data/fixture.sqlite');
for(const role of ['OPR','SUP','L2','L3','CMR','AUD','ADM'])if(!d.one('SELECT id FROM users WHERE email=?',role.toLowerCase()+'@example.test'))d.userCreate(role.toLowerCase()+'@example.test',pw,role);
if(!d.one('SELECT id FROM tickets LIMIT 1')){for(const title of ['QRIS notifikasi tertunda','Permintaan panduan settlement','Akses akun merchant contoh'])d.intake({merchant:'Merchant sintetis',pic:'PIC sintetis',category:'QRIS',type:'Incident',title,description:'DATA FIXTURE LOKAL — seluruh informasi sintetis.',contact:'merchant@example.test'},id(),true);d.processIntakes();}
if(!d.one('SELECT id FROM knowledge LIMIT 1')){d.run('INSERT INTO knowledge(title,body,kind) VALUES(?,?,?)','Pemeriksaan QRIS','Periksa referensi transaksi melalui kanal resmi. Jangan meminta PIN, OTP, atau nomor identitas.','article');d.run('INSERT INTO knowledge(title,body,kind) VALUES(?,?,?)','Konfirmasi penerimaan','Terima kasih. Laporan Anda sedang kami periksa. Mohon simpan nomor tiket untuk tindak lanjut.','macro');}
d.db.close();console.log('Fixture sintetis siap: akun <role>@example.test; password dari environment, tidak dicetak.');
