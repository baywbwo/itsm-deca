import {it,expect} from 'vitest';
import {createApi,MfaChallenge,type TransportRequest} from './client';
it('preserves MFA challenge and submits second factor without pre-session CSRF',async()=>{
 const seen:TransportRequest[]=[];
 const api=createApi(async req=>{seen.push(req);if(req.path==='/api/v1/login')return {status:401,body:{error:{code:'mfa_required',message:'MFA',challengeId:'challenge-1'}},requestId:null};return {status:200,body:{user:{id:'u1',displayName:'Admin'},banner:null,uatSuperadmin:false,csrfToken:'csrf',mfaRequired:true,mfaEnrolled:true},requestId:null};});
 await expect(api.login('admin','password')).rejects.toBeInstanceOf(MfaChallenge);
 await expect(api.loginMfa('challenge-1','123456')).resolves.toMatchObject({mfaEnrolled:true});
 expect(seen.map(r=>r.path)).toEqual(['/api/v1/login','/api/v1/login/mfa']);
 expect(seen[1]?.body).toEqual({challengeId:'challenge-1',code:'123456'});
 expect(api.hasCsrf()).toBe(true);
});
