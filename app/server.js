import http from 'node:http';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { createDomain,fail,id,hash,verify,csvCell,redact,taxonomy,mask,calendarDate,validateAttachment } from './domain.js';
export function createApp({dbPath=':memory:',now=()=>Date.now(),secure=false,fixture=false}={}){
 if(fixture&&process.env.NODE_ENV==='production')throw new Error('Fixture disabled in production');
 const d=createDomain(dbPath,now),limits=new Map();d.dummyPassword=password(id());
 function limit(key,max,window=60000){const entry=limits.get(key);if(!entry||entry.at+window<=now()){limits.set(key,{at:now(),n:1});return;}if(++entry.n>max)fail(429,'Terlalu banyak percobaan; coba kembali nanti');if(limits.size>10000)for(const [k,v] of limits)if(v.at+window<now())limits.delete(k);}
 const server=http.createServer(async(req,res)=>{
 const requestId=id();res.setHeader('X-Request-ID',requestId);res.setHeader('X-Content-Type-Options','nosniff');res.setHeader('Referrer-Policy','no-referrer');res.setHeader('Cache-Control','no-store');res.setHeader('Content-Security-Policy',"default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'");
 const send=(status,data,type='application/json')=>{res.writeHead(status,{'Content-Type':type+'; charset=utf-8'});res.end(type==='application/json'?JSON.stringify(data):data);};
 try{
 const url=new URL(req.url,'http://localhost'),p=url.pathname,method=req.method;let b={};
 if(['POST','PATCH','PUT','DELETE'].includes(method)){
 if(req.headers.origin&&req.headers.origin!==(secure?'https://':'http://')+req.headers.host)fail(403,'Origin tidak diizinkan');
 if(req.headers['sec-fetch-site']==='cross-site')fail(403,'Origin tidak diizinkan');
 let bytes=0,chunks=[];for await(const c of req){bytes+=c.length;if(bytes>7200000)fail(413,'Isi terlalu besar');chunks.push(c);}if(bytes){if(!req.headers['content-type']?.startsWith('application/json'))fail(415,'Gunakan JSON');try{b=JSON.parse(Buffer.concat(chunks).toString());}catch{fail(400,'JSON tidak valid');}if(!b||typeof b!=='object'||Array.isArray(b))fail(400,'Objek JSON wajib');}
 }
 if(method==='GET'&&p==='/healthz')return send(200,{status:'ok',schema:1});
 if(method==='GET'&&p==='/version')return send(200,{name:'DOB ITSM Platform',version:'0.1.0',commit:process.env.APP_COMMIT||'development'});
 if(method==='GET'&&p==='/api/intake-schema')return send(200,{categories:taxonomy.categories,types:['Incident','Service Request'],limits:{merchant:120,pic:80}});
 if(method==='POST'&&p==='/api/intake'){
 const ip=req.socket.remoteAddress;limit('intake:'+ip,10);const key=req.headers['idempotency-key'];const old=typeof key==='string'&&d.one('SELECT id FROM intakes WHERE key=?',key);if(!old)limit('contact:'+hash(String(b.contact)),3);const result=d.intake(b,key,fixture);if(result.replay)res.setHeader('Idempotent-Replay','true');const {replay,...data}=result;return send(replay?200:202,data);}
 if(method==='GET'&&p.startsWith('/api/intake/')){limit('poll:'+req.socket.remoteAddress,120);const intake=d.one('SELECT * FROM intakes WHERE id=?',p.split('/').pop());if(!intake||intake.created<now()-7*86400000)fail(404,'Intake tidak ditemukan');return send(200,intake.ticket_id?{status:'TIKET_DIBUAT',ticket_no:intake.ticket_id}:{status:'DITERIMA'});}
 if(method==='POST'&&p==='/api/login'){
 limit('login:'+req.socket.remoteAddress+':'+String(b.email).slice(0,200),5,15*60000);limit('login-ip:'+req.socket.remoteAddress,50,15*60000);
 const user=d.one('SELECT * FROM users WHERE email=?',String(b.email||''));let valid=false;if(typeof b.password==='string'&&b.password.length<=1024){if(user)valid=verify(b.password,user.password);else verify(b.password,d.dummyPassword);}if(!valid){d.audit(null,'login.failed','auth');fail(401,'Email atau password salah');}
 const token=id(),cookie=req.headers.cookie?.match(/(?:^|; )sid=([a-f0-9]+)/)?.[1];d.transaction(()=>{if(cookie)d.run('DELETE FROM sessions WHERE token=?',hash(cookie));d.run('INSERT INTO sessions VALUES(?,?,?,?)',hash(token),user.id,now()+8*3600000,now());d.audit(user.id,'login','auth');});res.setHeader('Set-Cookie',`sid=${token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=28800${secure?'; Secure':''}`);return send(200,{id:user.id,email:user.email,role:user.role});}
 if(method==='GET'&&['/','/app.js','/draft-state.js','/style.css'].includes(p)){const f=p==='/'?'index.html':p.slice(1);return send(200,readFileSync(new URL('./public/'+f,import.meta.url),'utf8'),f.endsWith('.js')?'text/javascript':f.endsWith('.css')?'text/css':'text/html');}
 const token=req.headers.cookie?.match(/(?:^|; )sid=([a-f0-9]+)/)?.[1];const user=token&&d.one('SELECT u.id,u.email,u.role,s.last_seen,s.expires FROM sessions s JOIN users u ON u.id=s.user_id WHERE s.token=?',hash(token));if(!user||user.expires<=now()||user.last_seen<now()-(['CMR','L3'].includes(user.role)?15:30)*60000)fail(401,'Sesi berakhir; masuk kembali');d.run('UPDATE sessions SET last_seen=? WHERE token=?',now(),hash(token));
 if(method==='GET'&&p==='/api/me')return send(200,{id:user.id,email:user.email,role:user.role,fixture,permissions:Object.keys(awaitRights).filter(k=>(awaitRights[k]||[]).includes(user.role))});
 if(method==='POST'&&p==='/api/logout'){d.transaction(()=>{d.run('DELETE FROM sessions WHERE token=?',hash(token));d.audit(user.id,'logout','auth');});res.setHeader('Set-Cookie','sid=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0');return send(200,{ok:true});}
 if(method==='GET'&&p==='/api/config')return send(200,taxonomy);
 if(method==='GET'&&p==='/api/users'){d.allow(user,'assign');return send(200,d.all("SELECT id,email,role FROM users WHERE role IN ('OPR','SUP','L2','L3')"));}
 if(method==='GET'&&p==='/api/tickets'){
 d.allow(user,'tickets');const q=(url.searchParams.get('q')||'').slice(0,200),status=url.searchParams.get('status')||'',mine=url.searchParams.get('mine')==='1',triage=url.searchParams.get('triage')==='1';const rows=d.all('SELECT * FROM tickets WHERE (title LIKE ? OR CAST(id AS TEXT)=?) AND (?=\'\' OR status=?) AND (?=0 OR owner=?) AND (?=0 OR triaged IS NULL) ORDER BY updated DESC LIMIT 200','%'+q+'%',q,status,status,+mine,user.id,+triage);return send(200,{items:rows.map(t=>d.visible(t,user)),limit:200});}
 const match=p.match(/^\/api\/tickets\/(\d+)(?:\/(messages|history|promote|attachments))?$/);
 if(match){const tid=Number(match[1]),sub=match[2];
 if(method==='GET'&&!sub){d.allow(user,'tickets');return send(200,d.visible(d.ticket(tid),user));}
 if(method==='PATCH'&&!sub)return send(200,d.update(user,tid,b));
 if(sub==='messages'){d.allow(user,'tickets');if(method==='GET'){if(['CMR','AUD'].includes(user.role))fail(403,'Percakapan tidak diizinkan');return send(200,d.all('SELECT * FROM messages WHERE ticket_id=?',tid).map(m=>user.role==='L3'?{...m,body:mask(m.body)}:m));}if(method==='POST')return send(201,d.message(user,tid,b.kind,b.body));}
 if(sub==='history'&&method==='GET'){d.allow(user,'history');return send(200,d.all('SELECT id,actor,action,created FROM audit WHERE object=? ORDER BY id',String(tid)));}
 if(sub==='promote'&&method==='POST')return send(201,d.promote(user,tid));
 if(sub==='attachments'){d.allow(user,'tickets');if(['L3','CMR','AUD'].includes(user.role))fail(403,'Lampiran mentah tidak tersedia untuk peran masked');d.ticket(tid);if(method==='GET')return send(200,d.all('SELECT id,name,mime,length(data) AS size,created FROM attachments WHERE ticket_id=?',tid));if(method==='POST'){d.allow(user,'reply');if(d.one('SELECT COUNT(*) AS n FROM attachments WHERE ticket_id=?',tid).n>=5)fail(400,'Maksimum 5 berkas');const data=validateAttachment(b);const aid=id();d.transaction(()=>{d.run('INSERT INTO attachments VALUES(?,?,?,?,?,?)',aid,tid,redact(b.name),'application/octet-stream',data,now());d.audit(user.id,'attachment.add',tid,{attachment:aid});d.evidenceChanged(user,tid);});return send(201,{id:aid,scan:'Belum dipindai malware'});}}
 }
 if(method==='GET'&&p.startsWith('/api/attachments/')){d.allow(user,'tickets');if(['L3','CMR','AUD'].includes(user.role))fail(403,'Lampiran mentah tidak tersedia untuk peran masked');const a=d.one('SELECT * FROM attachments WHERE id=?',p.split('/').pop());if(!a)fail(404,'Lampiran tidak ditemukan');res.writeHead(200,{'Content-Type':'application/octet-stream','Content-Disposition':`attachment; filename="download"; filename*=UTF-8''${encodeURIComponent(a.name)}`});res.end(a.data);return;}
 if(method==='GET'&&p==='/api/knowledge'){d.allow(user,'tickets');return send(200,d.all('SELECT * FROM knowledge WHERE title LIKE ? OR body LIKE ? LIMIT 100','%'+(url.searchParams.get('q')||'')+'%','%'+(url.searchParams.get('q')||'')+'%'));}
 if(method==='GET'&&p==='/api/dashboard'){d.allow(user,'tickets');const rows=d.all('SELECT * FROM tickets');return send(200,{total:rows.length,open:rows.filter(t=>!['Teratasi','Ditutup','Digabung'].includes(t.status)).length,sla_unverified:rows.filter(t=>!d.sla(t).history_complete).length,breached:rows.filter(t=>!['Teratasi','Ditutup','Digabung'].includes(t.status)&&d.sla(t).remaining_minutes<0).length,triage:rows.filter(t=>!t.triaged).length,by_status:d.all('SELECT status,COUNT(*) AS count FROM tickets GROUP BY status'),fixture,definition:'Data lokal; SLA menit kerja, bukan SLA resmi'});}
 if(p==='/api/reports'&&method==='GET'){d.allow(user,'reports');return send(200,d.all('SELECT r.*,t.title,t.created,t.is_test FROM reports r JOIN tickets t ON t.id=r.ticket_id').map(r=>({...r,title:mask(r.title),note:r.note===null?null:mask(r.note)})));}
 if(/^\/api\/reports\/\d+\/approve$/.test(p)&&method==='POST')return send(200,d.approve(user,Number(p.split('/')[3]),b));
 if(p==='/api/reconciliation'&&method==='POST'){
 d.allow(user,'reports');const from=calendarDate(b.from),until=calendarDate(b.until);if(until<=from)fail(400,'Periode tidak valid');
 const source=d.all('SELECT id FROM tickets WHERE complaint=1 AND created>=? AND created<? AND is_test=0',from,until).map(t=>t.id);
 const exported=d.all('SELECT t.id FROM reports r JOIN tickets t ON t.id=r.ticket_id WHERE t.created>=? AND t.created<? AND t.is_test=0',from,until).map(t=>t.id);
 const missing=source.filter(id=>!exported.includes(id)),extra=exported.filter(id=>!source.includes(id));d.audit(user.id,'reconciliation.preview','reports',{from,until});
 return send(200,{status:'REKONSILIASI TIDAK LENGKAP',checked_at:now(),from:b.from,until:b.until,local_source_count:source.length,export_count:exported.length,missing_ids:missing,extra_ids:extra,export_ids:exported,sources:[{name:'Zammad',count:null,status:'Tidak tersedia — integrasi nyata belum dikonfigurasi'},{name:'Lark',count:null,status:'Tidak tersedia — integrasi nyata belum dikonfigurasi'}],items:d.all('SELECT r.ticket_id,r.status,r.version,t.title FROM reports r JOIN tickets t ON t.id=r.ticket_id WHERE t.created>=? AND t.created<? AND t.is_test=0',from,until).map(r=>({...r,title:mask(r.title)}))});
 }
 if(p==='/api/export'&&method==='POST'){
 d.allow(user,'reports');limit('export:'+user.id,10);if(!b.reason?.trim()||!b.purpose?.trim()||!/^\d{4}-\d{2}-\d{2}$/.test(b.from||'')||!/^\d{4}-\d{2}-\d{2}$/.test(b.until||''))fail(400,'Alasan, tujuan dan periode wajib');const from=calendarDate(b.from),until=calendarDate(b.until);if(!Number.isFinite(from)||!Number.isFinite(until)||until<=from)fail(400,'Periode tidak valid');const rows=d.all('SELECT r.*,t.title,t.category,t.severity,t.created,t.complaint_status FROM reports r JOIN tickets t ON t.id=r.ticket_id WHERE t.created>=? AND t.created<? AND t.is_test=0',from,until);const eid=id();const aid=d.audit(user.id,'export',eid,{reason:redact(b.reason),purpose:redact(b.purpose),from,until,row_count:rows.length});const csv=[['DATA_DRAFT — bukan laporan resmi','Asia/Jakarta','internal, belum diverifikasi resmi'],['export_id','source_id','audit_id','judul','kategori','severity','status_pengaduan','status_record','identitas'],...rows.map(r=>[eid,r.ticket_id,Number(aid.lastInsertRowid),mask(r.title),r.category,r.severity,r.complaint_status,r.status,'tidak tersedia'])].map(r=>r.map(csvCell).join(',')).join('\r\n');res.setHeader('X-Export-ID',eid);res.setHeader('X-Row-Count',rows.length);res.setHeader('X-Checksum-SHA256',hash(csv));res.setHeader('Content-Disposition','attachment; filename="dob-data-draft.csv"');return send(200,csv,'text/csv');}
 if(p==='/api/audit'&&method==='GET'){d.allow(user,'audit');d.audit(user.id,'audit.read','audit');return send(200,d.all('SELECT * FROM audit ORDER BY id DESC LIMIT 200'));}
 if(p==='/api/outbox'&&method==='GET'){d.allow(user,'outbox');return send(200,{mode:'Simulasi lokal',items:d.all('SELECT * FROM outbox ORDER BY id DESC LIMIT 200')});}
 if(/^\/api\/outbox\/\d+\/retry$/.test(p)&&method==='POST'){d.allow(user,'retry');const oid=Number(p.split('/')[3]);const o=d.one('SELECT * FROM outbox WHERE id=?',oid);if(!o||o.state!=='dead')fail(409,'Hanya antrean dead dapat diulang');d.transaction(()=>{d.run("UPDATE outbox SET state='pending',attempts=0,next_at=? WHERE id=?",now(),oid);d.audit(user.id,'outbox.retry',oid);});return send(200,{ok:true});}
 fail(404,'Halaman tidak ditemukan');
 }catch(e){const status=e.status||500;if(status===429)res.setHeader('Retry-After','60');if(!res.headersSent)send(status,{error:status===500?'Kesalahan internal':e.message,request_id:requestId});else res.end();}
 });
 return {server,domain:d};
}
import { rights as awaitRights,password } from './domain.js';
// Constant-cost dummy for unknown-user login, generated once per process.
if(process.argv[1]===fileURLToPath(import.meta.url)){
 const {server,domain}=createApp({dbPath:process.env.APP_DB||'app/data/itsm.sqlite',secure:process.env.COOKIE_SECURE==='1',fixture:process.env.APP_FIXTURES==='1'});domain.dummyPassword=password(id());const job=setInterval(()=>{try{domain.jobs();}catch{console.error('worker failed');}},1000);server.listen(Number(process.env.PORT||3100),process.env.HOST||'127.0.0.1',()=>console.log('DOB ITSM Platform listening (local)'));for(const signal of ['SIGTERM','SIGINT'])process.on(signal,()=>{clearInterval(job);server.close(()=>{domain.db.close();process.exit(0);});});
}
