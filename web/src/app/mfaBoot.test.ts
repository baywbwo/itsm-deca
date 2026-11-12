import { describe, it, expect, vi } from 'vitest';
import { loadBoot } from './hooks';
import { ApiError, type Api } from '../api/client';
function fake(enrolled: boolean, workspaces = vi.fn().mockResolvedValue([])) {
 return { getMeta: vi.fn().mockResolvedValue({accessMode:'password'}), getSession: vi.fn().mockResolvedValue({mfaRequired:true,mfaEnrolled:enrolled}), listWorkspaces:workspaces } as unknown as Api;
}
describe('MFA boot gate',()=>{
 it('stops before workspace access for unenrolled mandatory MFA',async()=>{const api=fake(false);expect((await loadBoot(api,new AbortController().signal)).status).toBe('mfa_setup');expect(api.listWorkspaces).not.toHaveBeenCalled();});
 it('sends an old enrolled session to reauthentication on 403',async()=>{const api=fake(true,vi.fn().mockRejectedValue(new ApiError(403,'forbidden','Denied')));expect((await loadBoot(api,new AbortController().signal)).status).toBe('mfa_relogin');});
 it('allows a verified session through to its workspaces',async()=>{expect((await loadBoot(fake(true),new AbortController().signal)).status).toBe('ready');});
});
