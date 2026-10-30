import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import {createDraftState} from '../public/draft-state.js';
import {createApp} from '../server.js';
// Small DOM double for event/state regression; not a browser/a11y renderer.
function harness(fetch){
 class Element {
  constructor(tag){this.tagName=tag;this.children=[];this.attrs={};this._value=undefined;this.text='';this.files=[];this.listeners={};}
  set textContent(v){this.text=String(v);this.children=[];}get textContent(){return this.text+this.children.map(n=>n.textContent).join('');}
  set value(v){this._value=String(v);}get value(){return this._value??(this.tagName==='select'?this.children[0]?.getAttribute('value')||'':'');}
  setAttribute(k,v){this.attrs[k]=String(v);if(k==='name')this.name=String(v);if(k==='type')this.type=v;}
  getAttribute(k){return this.attrs[k]??null;}removeAttribute(k){delete this.attrs[k];}
  append(...nodes){for(const n of nodes){n.parent=this;this.children.push(n);}}prepend(n){n.parent=this;this.children.unshift(n);}replaceChildren(...nodes){this.children=[];this.text='';this.append(...nodes);}remove(){if(this.parent)this.parent.children=this.parent.children.filter(n=>n!==this);}
  matches(s){if(s.startsWith('#'))return this.attrs.id===s.slice(1);if(s.startsWith('.'))return (this.attrs.class||'').split(' ').includes(s.slice(1));if(s.startsWith('[')){const [k,v]=s.slice(1,-1).split('=');return v===undefined?this.attrs[k]!==undefined:this.attrs[k]===v;}return this.tagName===s;}
  querySelectorAll(s){return this.children.flatMap(n=>[...(n.matches(s)?[n]:[]),...n.querySelectorAll(s)]);}querySelector(s){return this.querySelectorAll(s)[0]||null;}
  get elements(){const all=this.querySelectorAll('[name]');all.namedItem=name=>all.find(n=>n.name===name);return all;}
  addEventListener(name,fn){(this.listeners[name]??=[]).push(fn);}dispatchEvent(e){for(const fn of this.listeners[e.type]||[])fn(e);if(e.bubbles&&this.parent)this.parent.dispatchEvent(e);return true;}
  focus(){this.focused=true;}closest(s){return this.matches(s)?this:this.parent?.closest(s);}reset(){for(const n of this.elements)n.value='';}
 }
 const root=new Element('div');root.setAttribute('id','app');const notice=new Element('div');notice.setAttribute('id','notice');const body=new Element('body');body.append(root,notice);
 class FormData {constructor(f){this.rows=f.elements.filter(n=>n.type!=='checkbox'||n.checked).map(n=>[n.name,n.value]);} [Symbol.iterator](){return this.rows[Symbol.iterator]();}}
 const window={addEventListener(){},confirm:()=>true};const location={hash:'#intake'};
 const context=vm.createContext({document:{querySelector:s=>body.querySelector(s),createElement:t=>new Element(t),body,title:''},window,location,fetch,FormData,crypto:globalThis.crypto,Event:class{constructor(type,opts={}){this.type=type;Object.assign(this,opts);}},createDraftState,URL,URLSearchParams,console,setTimeout:()=>0});
 let source=readFileSync(new URL('../public/app.js',import.meta.url),'utf8').replace("import {createDraftState} from './draft-state.js';",'');source=source.slice(0,source.indexOf("window.addEventListener('hashchange'"));
 vm.runInContext(source+"\nglobalThis.ui={intake,tickets,ticket,reports,knowledge,skipToMain,shell,error,form,draftState,setUser(u){adoptUser(u);version={version:'0.1.0',commit:'test'};config={};}};",context);
 return {ui:context.ui,root,body,window,location,Element,input(f,name,value){const n=f.elements.namedItem(name);n.value=value;n.dispatchEvent(new context.Event('input',{bubbles:true}));},submit:f=>f.onsubmit({preventDefault(){}})};
}
test('D10-01/03 intake receipt loss, navigation, retry one HTTP intake; schema failure is recoverable',async t=>{
 const app=createApp();await new Promise(r=>app.server.listen(0,'127.0.0.1',r));t.after(()=>new Promise(r=>app.server.close(()=>{app.domain.db.close();r();})));
 const base='http://127.0.0.1:'+app.server.address().port;let lose=true,schemaFail=false,releaseLost,acceptedResolve;const accepted=new Promise(resolve=>acceptedResolve=resolve);const keys=[];
 const h=harness(async(path,options)=>{if(path==='/api/intake-schema'&&schemaFail)return new Response(JSON.stringify({error:'schema unavailable'}),{status:500});const r=await fetch(base+path,options);if(path==='/api/intake'){keys.push(options.headers['Idempotency-Key']);if(lose){lose=false;await r.text();acceptedResolve();await new Promise(resolve=>releaseLost=resolve);throw Error('response lost after acceptance');}}return r;});
 await h.ui.intake();let f=h.root.querySelector('form');for(const [name,value]of Object.entries({merchant:'Synthetic',pic:'Synthetic PIC',title:'Synthetic',description:'Synthetic chronology',contact:'merchant@example.test'}))h.input(f,name,value);
 const pending=h.submit(f);await accepted;assert.equal(h.ui.draftState.drafts.get('public|#intake|Kirim laporan').title,'Synthetic');releaseLost();await pending;assert.equal(app.domain.one('SELECT COUNT(*) n FROM intakes').n,1);assert.match(h.root.textContent,/response lost/);assert.equal(h.ui.draftState.dirty,true);
 h.location.hash='#login';h.root.replaceChildren();h.location.hash='#intake';schemaFail=true;await h.ui.intake();assert.match(h.root.textContent,/Gagal memuat formulir/);assert.ok(h.root.querySelectorAll('button').some(n=>n.textContent==='Coba lagi'));assert.equal(h.ui.draftState.dirty,true);
 schemaFail=false;await h.ui.intake();f=h.root.querySelector('form');assert.equal(f.elements.namedItem('title').value,'Synthetic');await h.submit(f);assert.equal(keys.length,2);assert.equal(keys[0],keys[1]);assert.equal(app.domain.one('SELECT COUNT(*) n FROM intakes').n,1);assert.match(h.root.textContent,/Laporan diterima/);assert.equal(h.ui.draftState.dirty,false);
});
test('D10-02 logout cancellation/failure retain draft; success clears RAM; D10-05 errors clear on input',async()=>{
 let calls=0,fail=true;const h=harness(async()=>{calls++;return new Response(JSON.stringify(fail?{error:'audit fault'}:{ok:true}),{status:fail?500:200});});h.ui.setUser({id:'operator',role:'OPR',email:'opr@example.test',permissions:[]});h.ui.draftState.drafts.set('operator|#ticket/1|Simpan pesan',{body:'Synthetic draft'});h.ui.shell();let logout=h.root.querySelectorAll('button').find(n=>n.textContent==='Keluar');h.window.confirm=()=>false;await logout.onclick();assert.equal(calls,0);assert.equal(h.ui.draftState.dirty,true);
 h.window.confirm=()=>true;await logout.onclick();assert.equal(calls,1);assert.match(h.root.textContent,/Keluar gagal/);assert.equal(h.ui.draftState.dirty,true);fail=false;await logout.onclick();assert.equal(h.ui.draftState.dirty,false);assert.match(h.root.textContent,/Masuk ke ruang kerja/);
 const parent=new h.Element('section'),f=h.ui.form(parent,'Simpan',async()=>{}),a=new h.Element('input'),b=new h.Element('input');a.setAttribute('name','title');b.setAttribute('name','contact');f.append(a,b);f.finish();h.ui.error(f,'title invalid');assert.equal(a.getAttribute('aria-invalid'),'true');h.ui.error(f,'contact invalid');assert.equal(a.getAttribute('aria-describedby'),null);assert.equal(b.getAttribute('aria-invalid'),'true');h.input(f,'contact','valid');assert.equal(b.getAttribute('aria-invalid'),null);assert.equal(b.getAttribute('aria-describedby'),null);assert.equal(f.querySelector('.error'),null);
});
test('D10-04 restored search drives query; preview periods and changed-filter stale responses',async()=>{
 const queries=[];let resolvePreview;
 const h=harness(async(path,options)=>{if(path.startsWith('/api/tickets')){queries.push(path);return new Response(JSON.stringify({items:[{id:1,title:'Synthetic long '+ 'X'.repeat(200),category:'QRIS',severity:'S2',created:1,status:'Baru',sla:{paused:false,remaining_minutes:42}}],limit:200}));}if(path==='/api/reports')return new Response('[]');if(path==='/api/reconciliation')return new Promise(resolve=>{resolvePreview=()=>resolve(new Response(JSON.stringify({status:'REKONSILIASI TIDAK LENGKAP',from:'2026-10-01',until:'2026-11-01',checked_at:Date.now(),local_source_count:1,export_count:1,missing_ids:[],extra_ids:[],sources:[],items:[]})));});throw Error('Unexpected route '+path);});
 h.ui.setUser({id:'operator',role:'OPR',permissions:['reports']});h.location.hash='#tickets';h.ui.draftState.drafts.set('operator|#tickets|Cari',{q:'Synthetic QRIS',status:'Baru'});let m=new h.Element('main');await h.ui.tickets(m,'tickets');assert.match(m.textContent,/Status kerja: Baru/);assert.match(m.textContent,/SLA resolusi: 42 mnt tersisa/);assert.ok(m.textContent.includes('X'.repeat(200)));assert.equal(new URL('http://local'+queries[0]).searchParams.get('q'),'Synthetic QRIS');assert.equal(new URL('http://local'+queries[0]).searchParams.get('status'),'Baru');await h.submit(m.querySelector('form'));assert.equal(h.ui.draftState.drafts.get('operator|#tickets|Cari').q,'Synthetic QRIS');
 h.location.hash='#reports';m=new h.Element('main');await h.ui.reports(m);const f=m.querySelector('form');h.input(f,'from','2026-10-01');h.input(f,'until','2026-11-01');const preview=m.querySelectorAll('button').find(n=>n.textContent==='Periksa rekonsiliasi');let pending=preview.onclick();resolvePreview();await pending;assert.match(m.textContent,/Periode hasil WIB: \[2026-10-01, 2026-11-01\)/);
 pending=preview.onclick();h.input(f,'until','2026-12-01');resolvePreview();await pending;assert.match(m.textContent,/Pratinjau basi/);assert.doesNotMatch(m.textContent,/Periode hasil WIB/);
});
test('D10-05 macro appends text and synchronizes RAM draft across form reconstruction',async()=>{
 const h=harness(async(path)=>{const value=path.endsWith('/messages')?[]:path==='/api/knowledge'?[{id:1,kind:'macro',title:'Acknowledgement',body:'Synthetic macro'}]:{id:1,title:'Fixture',category:'QRIS',severity:'S3',created:1,description:'Synthetic',contact:'[masked]',intake_fields:'{}',status:'Baru',sla:{remaining_minutes:30}};return new Response(JSON.stringify(value));});
 // Expose actual ticket renderer to the harness without adding a production test API.
 h.ui.setUser({id:'operator',role:'L3',permissions:['note']});h.location.hash='#ticket/1';
 // ticket is bound below by harness exports, same unmodified renderer.
 const m=new h.Element('main');await h.ui.ticket(m,1);const f=m.querySelector('form');assert.equal(f.getAttribute('data-visibility'),'internal');assert.match(f.textContent,/Catatan internal — hanya tim/);h.input(f,'body','Existing draft');const macro=f.elements.namedItem('macro');macro.value='1';macro.onchange();assert.equal(f.elements.namedItem('body').value,'Existing draft\n\nSynthetic macro');assert.equal(h.ui.draftState.drafts.get('operator|#ticket/1|Simpan pesan').body,'Existing draft\n\nSynthetic macro');const next=new h.Element('main');await h.ui.ticket(next,1);assert.equal(next.querySelector('form').elements.namedItem('body').value,'Existing draft\n\nSynthetic macro');assert.equal(next.querySelector('form').getAttribute('data-visibility'),'internal');
});
test('UX11-01 late intake success/rejection cannot clear new-account drafts or revive old intents',async()=>{
 for(const outcome of ['success','rejection'])for(const transition of ['logout','account']){
  let release;const h=harness(async path=>{if(path==='/api/intake-schema')return new Response(JSON.stringify({types:['Incident'],categories:['QRIS'],limits:{merchant:100,pic:100}}));if(path==='/api/logout')return new Response('{"ok":true}');if(path==='/api/intake')return new Promise((resolve,reject)=>{release=()=>outcome==='success'?resolve(new Response('{"intake_id":"old-receipt"}')):reject(Error('late network failure'));});throw Error(path);});
  h.ui.setUser({id:'old',role:'OPR',permissions:[]});await h.ui.intake();const f=h.root.querySelector('form');h.input(f,'title','Old synthetic draft');const pending=h.submit(f);while(!release)await new Promise(r=>setImmediate(r));
  if(transition==='logout'){h.ui.shell();await h.root.querySelectorAll('button').find(n=>n.textContent==='Keluar').onclick();assert.equal(h.ui.draftState.dirty,false);}
  h.ui.setUser({id:'new',role:'OPR',permissions:[]});h.ui.draftState.drafts.set('new|#intake|Kirim laporan',{title:'New draft'});const newIntent=h.ui.draftState.intent('new|#intake|Kirim laporan');release();await pending;
  assert.equal(h.ui.draftState.drafts.size,1,outcome+'/'+transition);assert.equal(h.ui.draftState.drafts.get('new|#intake|Kirim laporan').title,'New draft');assert.equal(h.ui.draftState.intent('new|#intake|Kirim laporan'),newIntent);assert.doesNotMatch(h.root.textContent,/old-receipt/);
  // Detached old form event must not write RAM after the lifecycle changed.
  h.input(f,'title','Detached stale input');await h.submit(f);assert.equal(h.ui.draftState.drafts.size,1);
 }
});
test('UX11-01 expiry retains same-account draft and intent; account switch discards them',async()=>{
 let expired=true;const keys=[];const h=harness(async path=>{if(path==='/api/intake-schema')return new Response(JSON.stringify({types:['Incident'],categories:['QRIS'],limits:{merchant:100,pic:100}}));if(path==='/api/intake'){keys.push(h.ui.draftState.intent('original|#intake|Kirim laporan'));return new Response(JSON.stringify(expired?{error:'expired'}:{intake_id:'receipt'}),{status:expired?401:200});}throw Error(path);});
 h.ui.setUser({id:'original',permissions:[]});await h.ui.intake();let f=h.root.querySelector('form');h.input(f,'title','Recoverable synthetic');await h.submit(f);assert.match(h.root.textContent,/Sesi berakhir/);assert.equal(h.ui.draftState.dirty,true);
 h.ui.setUser({id:'original',permissions:[]});expired=false;await h.ui.intake();f=h.root.querySelector('form');assert.equal(f.elements.namedItem('title').value,'Recoverable synthetic');await h.submit(f);assert.equal(keys[0],keys[1]);assert.equal(h.ui.draftState.dirty,false);
 h.ui.draftState.drafts.set('original|draft',{body:'private'});h.ui.setUser({id:'other',permissions:[]});assert.equal(h.ui.draftState.dirty,false);
});
test('UX09-12 restored KB query survives initial error and retry; empty state replaces loading',async()=>{
 const queries=[];let fail=true;const h=harness(async path=>{queries.push(path);return new Response(JSON.stringify(fail?{error:'mock unavailable'}:[]),{status:fail?500:200});});h.ui.setUser({id:'opr',permissions:[]});h.location.hash='#knowledge';h.ui.draftState.drafts.set('opr|#knowledge|Cari',{q:'QRIS synthetic'});const m=new h.Element('main');await h.ui.knowledge(m);assert.match(m.textContent,/Gagal memuat pengetahuan/);assert.doesNotMatch(m.textContent,/Memuat pengetahuan…/);fail=false;await m.querySelectorAll('button').find(n=>n.textContent==='Coba lagi').onclick();assert.match(m.textContent,/Belum ada artikel/);assert.equal(queries.length,2);for(const q of queries)assert.equal(new URL('http://local'+q).searchParams.get('q'),'QRIS synthetic');
});
test('UX09-13 skip focuses main and prevents fragment/router navigation (DOM double)',()=>{
 const h=harness(async()=>new Response('{}'));h.ui.setUser({id:'opr',role:'OPR',permissions:[]});h.location.hash='#tickets';h.ui.shell();let prevented=false;h.ui.skipToMain({preventDefault(){prevented=true;}});assert.equal(prevented,true);assert.equal(h.location.hash,'#tickets');assert.equal(h.root.querySelector('#main').focused,true);assert.equal(h.root.querySelector('#main').getAttribute('tabindex'),'-1');
});
test('UX11-01 failed logout leaves pending-submit recovery active',async()=>{
 const h=harness(async()=>new Response('{"error":"audit fault"}',{status:500}));h.ui.setUser({id:'opr',role:'OPR',permissions:[]});const m=new h.Element('main');let reject;const f=h.ui.form(m,'Simpan pesan',()=>new Promise((_,r)=>reject=r));const input=new h.Element('textarea');input.setAttribute('name','body');f.append(input);f.finish();h.input(f,'body','Recover after fault');const pending=h.submit(f);h.ui.shell();await h.root.querySelectorAll('button').find(n=>n.textContent==='Keluar').onclick();assert.match(h.root.textContent,/Keluar gagal/);reject(Error('late failure'));await pending;assert.equal([...h.ui.draftState.drafts.values()][0].body,'Recover after fault');
});
