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
test('UX11-01 logout success must keep RAM empty when an older submit fails late',async()=>{
 const h=harness(async()=>new Response(JSON.stringify({ok:true})));
 h.ui.setUser({id:'operator',role:'OPR',email:'opr@example.test',permissions:[]});h.location.hash='#ticket/1';
 const m=new h.Element('main');let rejectPending;
 const f=h.ui.form(m,'Simpan pesan',()=>new Promise((resolve,reject)=>{rejectPending=reject;}));
 const a=new h.Element('textarea');a.setAttribute('name','body');f.append(a);f.finish();h.input(f,'body','Synthetic late failure draft');
 const pending=h.submit(f);h.ui.shell();await h.root.querySelectorAll('button').find(n=>n.textContent==='Keluar').onclick();
 assert.equal(h.ui.draftState.dirty,false,'logout initially clears RAM');
 rejectPending(Error('synthetic delayed transport failure'));await pending;
 assert.equal(h.ui.draftState.dirty,false,'late submit failure must not resurrect draft after logout');
});
