import {spawn,spawnSync} from 'node:child_process';
import {randomBytes} from 'node:crypto';
import {writeFileSync,copyFileSync} from 'node:fs';
import assert from 'node:assert/strict';
const cwd=import.meta.dirname, sha='a727f14c086d8e850add595d8758488d77a19e4f', db=cwd+'/fixture.sqlite', base='http://127.0.0.1:3103';
const env={...process.env,NODE_ENV:'development',APP_FIXTURES:'1',APP_DB:db,APP_COMMIT:sha,HOST:'127.0.0.1',PORT:'3103',NODE_OPTIONS:'--max-old-space-size=128'};
const password=randomBytes(24).toString('hex');
assert.equal(spawnSync(process.execPath,['app/fixture.js'],{cwd,env:{...env,FIXTURE_PASSWORD:password},stdio:'ignore'}).status,0);
let child,cookie='';
const log=s=>console.log(s);
async function req(path,method='GET',body,status=200){const r=await fetch(base+path,{method,headers:{'Content-Type':'application/json',...(cookie?{Cookie:cookie}:{}),...(path==='/api/intake'?{'Idempotency-Key':'dev-run-17-fixture'}:{})},body:body?JSON.stringify(body):undefined});assert.equal(r.status,status,method+' '+path);return r;}
async function start(path=db){child=spawn(process.execPath,['app/server.js'],{cwd,env:{...env,APP_DB:path},stdio:'ignore'});for(let i=0;i<60;i++){try{const r=await fetch(base+'/healthz');if(r.ok)return;}catch{}await new Promise(r=>setTimeout(r,100));}throw Error('Startup timeout');}
async function stop(){child.kill('SIGTERM');await new Promise(r=>child.once('exit',code=>{assert.equal(code,0);r();}));}
try{
await start();assert.equal((await (await req('/version')).json()).commit,sha);log('PASS startup /healthz /version SHA '+sha);
const login=await req('/api/login','POST',{email:'opr@example.test',password});cookie=login.headers.get('set-cookie').split(';')[0];assert.match(login.headers.get('set-cookie'),/HttpOnly; SameSite=Strict/);log('PASS login HTTP cookie');
const items=(await (await req('/api/tickets')).json()).items;assert.equal(items.length,3);const tid=items[0].id;
for(const kind of ['internal','reply'])await req('/api/tickets/'+tid+'/messages','POST',{kind,body:'DEV RUN 17 '+kind},201);
const messages=await (await req('/api/tickets/'+tid+'/messages')).json();assert.deepEqual(messages.map(m=>m.kind),['internal','reply']);log('PASS internal/public persisted distinct kinds');
const data=Buffer.from('synthetic fixture attachment\n');const attachment=await (await req('/api/tickets/'+tid+'/attachments','POST',{name:'fixture.txt',data:data.toString('base64')},201)).json();assert.equal(Buffer.from(await (await req('/api/attachments/'+attachment.id)).arrayBuffer()).compare(data),0);log('PASS attachment upload/download bytes');
const intake=await (await req('/api/intake','POST',{merchant:'Synthetic merchant',pic:'Synthetic PIC',category:'QRIS',type:'Incident',title:'DEV RUN 17',description:'Synthetic request',contact:'run17@example.test'},202)).json();let status;for(let i=0;i<30;i++){status=await (await req('/api/intake/'+intake.intake_id)).json();if(status.status==='TIKET_DIBUAT')break;await new Promise(r=>setTimeout(r,100));}assert.equal(status.status,'TIKET_DIBUAT');log('PASS public intake real background worker creates ticket');
assert.equal(spawnSync(process.execPath,['app/backup.js',cwd+'/backup.sqlite'],{cwd,env,stdio:'ignore'}).status,0);await stop();log('PASS online backup integrity and SIGTERM exit0');
copyFileSync(cwd+'/backup.sqlite',cwd+'/restored.sqlite');await start(cwd+'/restored.sqlite');assert.equal((await (await req('/api/tickets')).json()).items.length,4);assert.equal((await (await req('/api/tickets/'+tid+'/messages')).json()).length,2);assert.equal(Buffer.from(await (await req('/api/attachments/'+attachment.id)).arrayBuffer()).compare(data),0);log('PASS restart restored snapshot session/tickets/messages/attachment retained');
await req('/api/logout','POST',{});await req('/api/me','GET',undefined,401);log('PASS logout session invalidated');await stop();log('PASS restored process shutdown');
// Browser fixture remains running without a persisted password or session.
await start(cwd+'/restored.sqlite');child.unref();writeFileSync(cwd+'/runtime.json',JSON.stringify({sha,pid:child.pid,url:base,db:cwd+'/restored.sqlite',memoryLimitMB:128}));log('PASS review fixture running loopback3103; credentials ephemeral discarded');
}catch(e){if(child)child.kill('SIGTERM');console.error(e.message);process.exitCode=1;}
