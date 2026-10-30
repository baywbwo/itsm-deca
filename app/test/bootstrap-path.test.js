import {test} from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync,rmSync,symlinkSync,renameSync,mkdirSync,readFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {spawnSync} from 'node:child_process';
import {createDomain} from '../domain.js';
import {guardBootstrapPath} from '../bootstrap-path.js';
const pw='synthetic-bootstrap-path-25';
test('USER01 exact path rejects file/parent symlinks and missing DB without mutation',t=>{
 const root=mkdtempSync(join(tmpdir(),'path25-'));t.after(()=>rmSync(root,{recursive:true,force:true}));
 const folder=join(root,'real');mkdirSync(folder);const db=join(folder,'db.sqlite');createDomain(db).db.close();
 const before=readFileSync(db);symlinkSync(db,join(root,'alias.sqlite'));symlinkSync(folder,join(root,'alias-parent'));
 for(const path of [join(root,'alias.sqlite'),join(root,'alias-parent','db.sqlite'),join(root,'missing.sqlite'),folder,folder+'/../real/db.sqlite']){
 const r=spawnSync(process.execPath,['app/bootstrap-admin.js',path,'admin@example.test','Admin'],{input:pw,encoding:'utf8'});assert.equal(r.status,1,r.stdout);assert.deepEqual(readFileSync(db),before);
 }
 const d=createDomain(db);assert.equal(d.one('SELECT COUNT(*) n FROM users').n,0);assert.equal(d.one('SELECT COUNT(*) n FROM bootstrap_state').n,0);d.db.close();
 assert.equal(spawnSync(process.execPath,['app/bootstrap-admin.js',db,'admin@example.test','Admin'],{input:pw,encoding:'utf8'}).status,0);
});
test('USER01 guard detects replacement inode, symlink substitution and parent replacement before mutation',t=>{
 const root=mkdtempSync(join(tmpdir(),'identity25-'));t.after(()=>rmSync(root,{recursive:true,force:true}));
 for(const mode of ['inode','symlink','parent']){
 const parent=join(root,mode);mkdirSync(parent);const path=join(parent,'db.sqlite');createDomain(path).db.close();const before=readFileSync(path);const guard=guardBootstrapPath(path);
 try{if(mode==='parent'){renameSync(parent,parent+'-old');mkdirSync(parent);createDomain(path).db.close();}else{renameSync(path,path+'.old');if(mode==='symlink')symlinkSync(path+'.old',path);else createDomain(path).db.close();}
 assert.throws(()=>guard.check());assert.deepEqual(readFileSync(mode==='parent'?join(parent+'-old','db.sqlite'):path+'.old'),before);
 }finally{guard.close();}
 }
});
