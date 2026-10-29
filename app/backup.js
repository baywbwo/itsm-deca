import {DatabaseSync} from 'node:sqlite';
import {resolve} from 'node:path';
import {mkdirSync} from 'node:fs';
const source=process.env.APP_DB;if(!source||!process.argv[2])throw new Error('APP_DB wajib; argumen tujuan backup wajib');
const destination=resolve(process.argv[2]);mkdirSync(resolve(destination,'..'),{recursive:true,mode:0o700});
const db=new DatabaseSync(source);db.prepare('VACUUM INTO ?').run(destination);db.close();
const check=new DatabaseSync(destination,{readOnly:true});if(check.prepare('PRAGMA integrity_check').get().integrity_check!=='ok')throw new Error('Backup integrity failed');check.close();console.log('Backup integrity_check: ok');
