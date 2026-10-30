import {isAbsolute, resolve, dirname, parse} from 'node:path';
import {lstatSync, realpathSync, openSync, fstatSync, closeSync, constants} from 'node:fs';

// Keep an fd for identity checks. SQLite still opens by pathname: this is not
// openat-style confinement. The operator must control all parent directories.
export function guardBootstrapPath(path){
 if(typeof path!=='string'||!isAbsolute(path)||resolve(path)!==path)throw Error('Invalid exact path');
 const paths=[];for(let p=path;;p=dirname(p)){paths.unshift(p);if(p===parse(p).root)break;}
 const identity=s=>[s.dev,s.ino];
 const inspect=()=>paths.map((p,i)=>{const s=lstatSync(p,{bigint:true});if(s.isSymbolicLink()||(i===paths.length-1?!s.isFile():!s.isDirectory()))throw Error('Invalid resource');return identity(s);});
 const original=inspect();if(realpathSync(path)!==path)throw Error('Noncanonical resource');
 let fd=openSync(path,constants.O_RDONLY|constants.O_NOFOLLOW);
 const same=(a,b)=>a[0]===b[0]&&a[1]===b[1];
 function check(){const current=inspect();if(realpathSync(path)!==path||!current.every((s,i)=>same(s,original[i]))||!same(identity(fstatSync(fd,{bigint:true})),original.at(-1)))throw Error('Resource changed');}
 try{check();}catch(e){closeSync(fd);throw e;}
 return {check,close(){if(fd!==undefined){closeSync(fd);fd=undefined;}}};
}
