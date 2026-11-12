import { describe, it, expect, vi } from 'vitest';
import { loadBoot } from './hooks';
import { ApiError, type Api } from '../api/client';
// CP4f: MFA is off unless ITSM_MFA=on, so the gate only applies when the session reports mfaEnabled.
function fake(enrolled: boolean, workspaces = vi.fn().mockResolvedValue([]), mfaEnabled = true) {
 return { getMeta: vi.fn().mockResolvedValue({accessMode:'password'}), getSession: vi.fn().mockResolvedValue({mfaEnabled,mfaRequired:true,mfaEnrolled:enrolled}), listWorkspaces:workspaces } as unknown as Api;
}
describe('MFA boot gate',()=>{
 it('stops before workspace access for unenrolled mandatory MFA',async()=>{const api=fake(false);expect((await loadBoot(api,new AbortController().signal)).status).toBe('mfa_setup');expect(api.listWorkspaces).not.toHaveBeenCalled();});
 it('sends an old enrolled session to reauthentication on 403',async()=>{const api=fake(true,vi.fn().mockRejectedValue(new ApiError(403,'forbidden','Denied')));expect((await loadBoot(api,new AbortController().signal)).status).toBe('mfa_relogin');});
 it('allows a verified session through to its workspaces',async()=>{expect((await loadBoot(fake(true),new AbortController().signal)).status).toBe('ready');});
 it('ITSM_MFA off: an unenrolled admin goes straight to workspaces',async()=>{const api=fake(false,undefined,false);expect((await loadBoot(api,new AbortController().signal)).status).toBe('ready');expect(api.listWorkspaces).toHaveBeenCalled();});
 it('ITSM_MFA off: a 403 is a real error, never an MFA re-login',async()=>{await expect(loadBoot(fake(true,vi.fn().mockRejectedValue(new ApiError(403,'forbidden','Denied')),false),new AbortController().signal)).rejects.toBeInstanceOf(ApiError);});
});
