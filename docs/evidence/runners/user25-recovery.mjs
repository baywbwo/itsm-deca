// Run from checkout/archive: node docs/evidence/runners/user25-recovery.mjs /absolute/old-68476bb-archive
import assert from 'node:assert/strict';
import {mkdtempSync,rmSync,copyFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createApp} from '../../../app/server.js';
import {createDomain} from '../../../app/domain.js';
import {userService} from '../../../app/users.js';
const oldRoot=resolve(process.argv[2]);
const {createDomain:oldDomain}=await import(pathToFileURL(join(oldRoot,'app/domain.js')));
const {createApp:oldApp}=await import(pathToFileURL(join(oldRoot,'app/server.js')));
const root=mkdtempSync(join(tmpdir(),'recovery25-'));const path=join(root,'users.sqlite'),backup=join(root,'pre.sqlite'),restored=join(root,'restored.sqlite');
const pw='synthetic-recovery-user25',reset='synthetic-reset-user25';let app;
const start=async(factory,dbPath)=>{app=factory({dbPath});await new Promise(r=>app.server.listen(0,'127.0.0.1',r));return 'http://127.0.0.1:'+app.server.address().port;};
const stop=async()=>{const current=app;await new Promise(r=>current.server.close(r));current.domain.db.close();app=null;};
const login=(base,email,password)=>fetch(base+'/api/login',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({email,password})});
try{
 let d=oldDomain(path);const admin=d.userCreate('admin@example.test',pw,'ADM'),operator=d.userCreate('operator@example.test',pw,'OPR'),resetUser=d.userCreate('reset@example.test',pw,'OPR');
 const original=d.all('SELECT * FROM users ORDER BY id');d.db.prepare('VACUUM INTO ?').run(backup);d.db.close();
 let base=await start(createApp,path);d=app.domain;const service=userService(d);
 const created=service.create(admin,{name:'Post backup',email:'later@example.test',role:'OPR',password:pw});service.update(admin,operator.id,{name:'Disabled',email:operator.email,role:'OPR',active:false});service.reset(admin,resetUser.id,reset);
 assert.equal((await login(base,operator.email,pw)).status,401);assert.equal((await login(base,resetUser.email,pw)).status,401);assert.equal((await login(base,resetUser.email,reset)).status,200);assert.equal(d.one('SELECT must_change_password v FROM users WHERE id=?',resetUser.id).v,1);assert.ok(d.one('SELECT id FROM users WHERE id=?',created.id));
 await stop();await assert.rejects(()=>login(base,admin.email,pw));
 // Keep post-upgrade DB for investigation; restore to a fresh path, never copy WAL onto backup.
 copyFileSync(backup,restored);base=await start(oldApp,restored);d=app.domain;
 assert.deepEqual(d.all('SELECT * FROM users ORDER BY id'),original);assert.equal((await login(base,operator.email,pw)).status,200);assert.equal((await login(base,resetUser.email,pw)).status,200);assert.equal((await login(base,'later@example.test',pw)).status,401);assert.equal(d.one('PRAGMA integrity_check').integrity_check,'ok');await stop();await assert.rejects(()=>login(base,admin.email,pw));
 base=await start(createApp,restored);d=app.domain;
 for(const u of original){const actual=d.one('SELECT * FROM users WHERE id=?',u.id);for(const k of ['id','email','password','role'])assert.equal(actual[k],u[k]);assert.equal(actual.active,1);assert.equal(actual.must_change_password,0);}
 assert.equal(d.one('SELECT COUNT(*) n FROM bootstrap_state').n,0);assert.equal(d.one('PRAGMA integrity_check').integrity_check,'ok');assert.equal((await login(base,operator.email,pw)).status,200);await stop();
 d=createDomain(restored);assert.equal(d.one('PRAGMA integrity_check').integrity_check,'ok');assert.equal(d.one('SELECT COUNT(*) n FROM users').n,3);d.db.close();
 console.log('PASS backup → user create/deactivate/reset → stop/login closed → restore+old source → reupgrade/restart/integrity; post-backup writes intentionally lost');
}finally{if(app)await stop();rmSync(root,{recursive:true,force:true});}
