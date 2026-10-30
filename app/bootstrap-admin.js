import {DatabaseSync} from 'node:sqlite';
import {isAbsolute} from 'node:path';
import {statSync} from 'node:fs';
import {createDomain} from './domain.js';
import {userService} from './users.js';
// Password only via stdin, never argv/environment/output. DB must already exist.
let d;
try{
 const [dbPath,email,name]=process.argv.slice(2);
 if(!dbPath||!isAbsolute(dbPath)||!statSync(dbPath).isFile())throw Error();
 const probe=new DatabaseSync(dbPath,{readOnly:true});try{if(!probe.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='users'").get())throw Error();}finally{probe.close();}
 let secret='';for await(const chunk of process.stdin){secret+=chunk;if(secret.length>2048)throw Error();}
 secret=secret.replace(/\r?\n$/,'');d=createDomain(dbPath);userService(d).bootstrap({email,name,password:secret});secret='';console.log('Bootstrap admin berhasil. Tidak ada fixture/tiket dibuat.');
}catch{console.error('Bootstrap ditolak. Periksa DB existing, input dan status bootstrap/admin/email; tidak ada reset otomatis.');process.exitCode=1;}finally{d?.db.close();}
