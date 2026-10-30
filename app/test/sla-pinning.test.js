import test from 'node:test';
import {createHash} from 'node:crypto';
import assert from 'node:assert/strict';
import {mkdtempSync,rmSync,mkdirSync,writeFileSync,copyFileSync,readFileSync} from 'node:fs';
import {spawnSync} from 'node:child_process';
import {join} from 'node:path';
import {tmpdir} from 'node:os';
import {createDomain,taxonomy,id,businessMinutes} from '../domain.js';
const stamp=s=>Date.parse(s+'+07:00');
function fixture(path=':memory:'){
 let clock=stamp('2026-10-30T16:30:00');const d=createDomain(path,()=>clock),u={id:'operator',role:'OPR'};
 const submit=()=>{d.intake({merchant:'Synthetic',pic:'Synthetic',category:'QRIS',type:'Incident',title:'Synthetic',description:'Fixture',contact:'fixture@example.test'},id());d.processIntakes();return d.one('SELECT MAX(id) id FROM tickets').id;};
 const update=(tid,body)=>d.update(u,tid,{...body,version:d.ticket(tid).version});
 return {d,submit,update,set:s=>clock=stamp(s)};
}
test('CFG04 snapshot survives changed defaults; severity and worker use pinned targets',()=>{
 const f=fixture(),{d}=f,old=structuredClone(taxonomy);try{
 const tid=f.submit();f.update(tid,{action:'triage',type:'Incident',category:'QRIS',severity:'S2',complaint:false});const version=d.sla(d.ticket(tid)).calendar_version;
 taxonomy.businessHours.holidays.push('2026-11-02');taxonomy.severities.S2.resolution=1;
 f.set('2026-11-02T08:30:00');assert.equal(d.sla(d.ticket(tid)).used_minutes,60);assert.equal(d.sla(d.ticket(tid)).resolution_minutes,480);
 d.jobs();assert.equal(d.one('SELECT COUNT(*) n FROM alerts').n,0);
 const fresh=f.submit();assert.notEqual(d.sla(d.ticket(fresh)).calendar_version,version);
 f.update(tid,{action:'triage',type:'Incident',category:'QRIS',severity:'S1',complaint:false});assert.equal(d.sla(d.ticket(tid)).remaining_minutes,180);
 }finally{Object.assign(taxonomy,old);d.db.close();}
});
test('CFG05 pause, close, repeated reopen, first response, exact seconds and holiday',()=>{
 const f=fixture(),{d}=f;try{
 const tid=f.submit();const status=status=>f.update(tid,{action:'status',status,resolution_cause:'Synthetic fix'});
 status('Diproses');d.message({id:'operator',role:'OPR'},tid,'reply','Synthetic reply');const first=d.ticket(tid).first_response;
 f.set('2026-10-30T16:45:00');status('Menunggu Pelapor');f.set('2026-11-02T08:15:00');status('Diproses');assert.equal(d.sla(d.ticket(tid)).used_minutes,15);
 f.set('2026-11-02T08:30:00');status('Teratasi');const resolved=d.ticket(tid).resolved;
 f.set('2026-11-03T09:00:00');status('Ditutup');assert.equal(d.ticket(tid).resolved,resolved);assert.equal(d.sla(d.ticket(tid)).used_minutes,30);
 f.set('2026-11-04T08:00:00');status('Diproses');assert.equal(d.sla(d.ticket(tid)).used_minutes,30);
 f.set('2026-11-04T08:00:30');assert.equal(d.sla(d.ticket(tid)).used_minutes,30.5);status('Teratasi');
 f.set('2026-11-05T08:00:00');status('Diproses');d.jobs();assert.equal(d.ticket(tid).status,'Diproses');assert.equal(d.ticket(tid).reopen_count,2);assert.equal(d.ticket(tid).first_response,first);assert.equal(d.sla(d.ticket(tid)).used_minutes,30.5);
 }finally{d.db.close();}
});
test('legacy additive migration reconstructs resolved interval from audit; restart idempotent and backup recovery',()=>{
 const dir=mkdtempSync(join(tmpdir(),'sla-migrate-')),path=join(dir,'db');let f=fixture(path),d=f.d;try{
 const tid=f.submit();const status=status=>f.update(tid,{action:'status',status,resolution_cause:'Synthetic fix'});
 status('Diproses');f.set('2026-10-30T16:45:00');status('Teratasi');f.set('2026-11-02T08:30:00');status('Diproses');
 d.db.exec('ALTER TABLE tickets DROP COLUMN sla_policy; ALTER TABLE tickets DROP COLUMN stopped_minutes; ALTER TABLE tickets DROP COLUMN reopen_count');
 const backup=join(dir,'before.sqlite');d.run('VACUUM INTO ?',backup);d.db.close();
 d=createDomain(path,()=>stamp('2026-11-02T08:30:00'));assert.equal(d.sla(d.ticket(tid)).used_minutes,15);assert.equal(d.ticket(tid).reopen_count,1);assert.equal(d.sla(d.ticket(tid)).policy_origin,'legacy-baseline');const pinned=d.ticket(tid).sla_policy;d.db.close();
 d=createDomain(path);assert.equal(d.ticket(tid).sla_policy,pinned);assert.equal(d.ticket(tid).reopen_count,1);assert.equal(d.one('PRAGMA integrity_check').integrity_check,'ok');
 // Run the actual previous domain against the pre-migration backup, without changing checkout.
 mkdirSync(join(dir,'app'));mkdirSync(join(dir,'config'));copyFileSync('config/taxonomy.json',join(dir,'config/taxonomy.json'));
 const previous=readFileSync(new URL('./fixtures/domain-27aafb0.txt',import.meta.url));assert.equal(createHash('sha256').update(previous).digest('hex'),'335893a78a1a298f2bd5211e5bfad09a618361749daca937f0ee2900518d74aa');writeFileSync(join(dir,'app','domain.mjs'),previous);
 const recovery=spawnSync(process.execPath,['--input-type=module','-e',"import {createDomain} from './app/domain.mjs';const d=createDomain('./before.sqlite');if(d.ticket(1).title!=='Synthetic'||d.one('PRAGMA integrity_check').integrity_check!=='ok')process.exit(1);d.db.close();"],{cwd:dir,encoding:'utf8'});assert.equal(recovery.status,0,recovery.stderr);
 const restored=createDomain(backup);assert.equal(restored.ticket(tid).title,'Synthetic');assert.equal(restored.ticket(tid).reopen_count,1);restored.db.close();
 }finally{if(d.db.isOpen)d.db.close();rmSync(dir,{recursive:true,force:true});}
});
test('200 independent day intersection cases: weekend, holiday, second boundaries',()=>{
 const calendar={start:8,end:17,weekdays:[1,2,3,4,5],holidays:['2026-11-02']};
 // Independent oracle intersects entire daily windows, never calls production minute loop.
 for(let i=0;i<200;i++){
 const from=stamp('2026-10-30T07:59:30')+i*173000,to=from+(i+1)*3601000;let expected=0;
 for(let day=Date.UTC(2026,9,29);day<to+86400000;day+=86400000){const date=new Date(day);if(calendar.weekdays.includes(date.getUTCDay())&&!calendar.holidays.includes(date.toISOString().slice(0,10)))expected+=Math.max(0,Math.min(to,day+10*3600000)-Math.max(from,day+3600000))/60000;}
 assert.ok(Math.abs(businessMinutes(from,to,calendar)-expected)<1e-8,'case '+i);
 }
});
test('reopen audit failure rolls back clock accumulators, version and outbox',()=>{
 const f=fixture(),{d}=f;try{const tid=f.submit();f.update(tid,{action:'status',status:'Diproses'});f.set('2026-10-30T16:45:00');f.update(tid,{action:'status',status:'Teratasi',resolution_cause:'Synthetic fix'});const before=d.ticket(tid),outbox=d.all('SELECT * FROM outbox');
 d.db.exec("CREATE TRIGGER fail_status BEFORE INSERT ON audit WHEN NEW.action='status' BEGIN SELECT RAISE(ABORT,'synthetic fault'); END");f.set('2026-11-02T08:30:00');assert.throws(()=>f.update(tid,{action:'status',status:'Diproses'}),/synthetic fault/);assert.deepEqual(d.ticket(tid),before);assert.deepEqual(d.all('SELECT * FROM outbox'),outbox);
 }finally{d.db.close();}
});
test('pinned holiday survives pause and reopen across year boundary',()=>{
 const old=structuredClone(taxonomy),f=fixture(),{d}=f;try{
 taxonomy.businessHours.holidays=['2027-01-01'];f.set('2026-12-31T16:59:30');const tid=f.submit();f.update(tid,{action:'status',status:'Diproses'});taxonomy.businessHours.holidays=[];
 f.set('2027-01-04T08:00:00');assert.equal(d.sla(d.ticket(tid)).used_minutes,0.5);f.update(tid,{action:'status',status:'Menunggu Pelapor'});
 f.set('2027-01-04T09:00:00');f.update(tid,{action:'status',status:'Teratasi',resolution_cause:'Synthetic fix'});f.set('2027-01-05T08:00:00');f.update(tid,{action:'status',status:'Diproses'});assert.equal(d.sla(d.ticket(tid)).used_minutes,0.5);
 }finally{Object.assign(taxonomy,old);d.db.close();}
});
test('legacy closed timestamp repaired from audit; unreadable history explicitly flagged',()=>{
 const dir=mkdtempSync(join(tmpdir(),'sla-legacy-')),path=join(dir,'db');const f=fixture(path);let d=f.d;try{
 const tid=f.submit();f.update(tid,{action:'status',status:'Diproses'});f.set('2026-10-30T16:45:00');f.update(tid,{action:'status',status:'Teratasi',resolution_cause:'Synthetic fix'});const resolved=d.ticket(tid).resolved;
 f.set('2026-11-02T08:30:00');f.update(tid,{action:'status',status:'Ditutup'});d.run('UPDATE tickets SET resolved=? WHERE id=?',stamp('2026-11-02T08:30:00'),tid);
 const broken=f.submit();d.run('INSERT INTO audit(actor,action,object,detail,created) VALUES(?,?,?,?,?)','fixture','status',String(broken),'unreadable legacy',resolved);
 d.db.exec('ALTER TABLE tickets DROP COLUMN sla_policy; ALTER TABLE tickets DROP COLUMN stopped_minutes; ALTER TABLE tickets DROP COLUMN reopen_count');d.db.close();d=createDomain(path);
 assert.equal(d.ticket(tid).resolved,resolved);assert.equal(d.sla(d.ticket(tid)).used_minutes,15);assert.equal(d.sla(d.ticket(broken)).policy_origin,'legacy-history-incomplete');
 }finally{if(d.db.isOpen)d.db.close();rmSync(dir,{recursive:true,force:true});}
});
test('QA20-D01 missing, partial, malformed and zero-event histories remain unverified across restart',()=>{
 for(const kind of ['missing','partial','malformed','gap','zero','new']){
 const dir=mkdtempSync(join(tmpdir(),'sla-incomplete-')),path=join(dir,'db');const f=fixture(path);let d=f.d;try{
 const tid=f.submit();const status=status=>f.update(tid,{action:'status',status,resolution_cause:'Synthetic'});
 if(!['zero','new'].includes(kind)){status('Diproses');f.set('2026-10-30T16:45:00');status('Teratasi');f.set('2026-11-02T08:30:00');status('Ditutup');f.set('2026-11-03T08:30:00');status('Diproses');}
 d.db.exec('DROP TRIGGER audit_no_delete');
 if(kind==='missing')d.run("DELETE FROM audit WHERE action='status'");
 if(kind==='partial')d.run("DELETE FROM audit WHERE id=(SELECT MAX(id) FROM audit WHERE action='status')");
 if(kind==='gap')d.run("DELETE FROM audit WHERE id=(SELECT MIN(id) FROM audit WHERE action='status')");
 if(kind==='malformed')d.run("INSERT INTO audit(actor,action,object,detail,created) VALUES('fixture','status',?,'{}',?)",String(tid),stamp('2026-11-03T08:30:00'));
 if(kind==='zero')d.run("UPDATE tickets SET status='Diproses' WHERE id=?",tid);
 const audit=d.all('SELECT * FROM audit');
 d.db.exec('ALTER TABLE tickets DROP COLUMN sla_policy; ALTER TABLE tickets DROP COLUMN stopped_minutes; ALTER TABLE tickets DROP COLUMN reopen_count');d.db.close();
 d=createDomain(path,()=>stamp('2026-11-03T08:30:00'));const metrics=d.sla(d.ticket(tid));
 assert.equal(metrics.history_complete,kind==='new',kind);
 if(kind!=='new'){assert.equal(metrics.used_minutes,null);assert.equal(metrics.remaining_minutes,null);assert.match(metrics.history_warning,/rekonsiliasi/);d.jobs();assert.equal(d.one('SELECT COUNT(*) n FROM alerts').n,0);}
 assert.deepEqual(d.all('SELECT * FROM audit'),audit);const snapshot=d.ticket(tid);d.db.close();d=createDomain(path);assert.deepEqual(d.ticket(tid),snapshot);
 }finally{if(d.db.isOpen)d.db.close();rmSync(dir,{recursive:true,force:true});}
 }
});
