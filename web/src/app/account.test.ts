import { describe, expect, it } from 'vitest';
import { ApiError, createApi, loginErrorText, type TransportRequest, type TransportResponse } from '../api/client';
import { passwordChangeError, validatePasswordChange } from './account';
import { loadBoot } from './hooks';

// CP3a (US-1.1, US-1.4, US-1.6): login, forced password change, logout everywhere and a session
// that ends in the middle of work. Fetch is mocked at the transport; nothing reaches a server.

type Route = (req: TransportRequest) => TransportResponse | undefined;
const res = (status: number, body: unknown = null): TransportResponse => ({ status, body, requestId: 'rid-1' });
const err = (status: number, code: string, extra: Record<string, unknown> = {}) =>
  res(status, { error: { code, message: 'server text', ...extra } });
const user = { id: 'u-1', displayName: 'Sari Wulandari' };
const meta = { environment: 'production', accessMode: 'password', banner: null, uat: null };
const issued = (mustChangePassword: boolean) =>
  res(201, { user, banner: null, uatSuperadmin: false, mustChangePassword, csrfToken: 'csrf-1' });

function mock(route: Route) {
  const requests: TransportRequest[] = [];
  const api = createApi(async (req) => {
    requests.push(req);
    const hit = route(req);
    if (!hit) throw new Error(`unexpected ${req.method} ${req.path}`);
    return hit;
  });
  return { api, requests, paths: () => requests.map((r) => `${r.method} ${r.path}`) };
}
const signal = () => new AbortController().signal;

describe('CP3a login (US-1.1)', () => {
  it('reads every refusal the same and shows when to retry on 429', async () => {
    const generic = loginErrorText(new ApiError(401, 'unauthenticated', 'x'));
    // Wrong password, unknown, locked and deactivated accounts all answer 401 from the server.
    for (const code of ['unauthenticated', 'forbidden', 'not_found'] as const) {
      expect(loginErrorText(new ApiError(code === 'unauthenticated' ? 401 : code === 'forbidden' ? 403 : 404, code, 'x'))).toBe(generic);
    }
    expect(generic).not.toMatch(/terkunci|nonaktif|tidak ditemukan/i);
    const limited = loginErrorText(new ApiError(429, 'rate_limited', 'x', null, null, 42));
    expect(limited).toContain('42 detik');
    expect(loginErrorText(new ApiError(0, 'network', 'x'))).toMatch(/tidak dapat dihubungi/);
  });

  it('sends the login header without CSRF, carries mustChangePassword and keeps the 429 wait', async () => {
    const { api, requests } = mock((r) => (r.path === '/api/v1/login' ? issued(true) : undefined));
    const s = await api.login('sari', 'synthetic-pass');
    expect(s.mustChangePassword).toBe(true);
    expect(requests[0]?.headers['X-ITSM-Login']).toBe('1');
    expect(requests[0]?.headers['X-CSRF-Token']).toBeUndefined();

    const limited = mock(() => err(429, 'rate_limited', { retryAfterSeconds: 7 }));
    const e = await limited.api.login('sari', 'x').catch((x: unknown) => x);
    expect(e).toBeInstanceOf(ApiError);
    expect((e as ApiError).retryAfterSeconds).toBe(7);
  });
});

describe('CP3a forced password change (US-1.4)', () => {
  it('boot stops at the change screen and never asks for workspaces while the flag is set', async () => {
    const { api, paths } = mock((r) => {
      if (r.path === '/api/v1/meta') return res(200, meta);
      if (r.path === '/api/v1/session') return res(200, { user, banner: null, uatSuperadmin: false, mustChangePassword: true });
      return undefined;
    });
    const boot = await loadBoot(api, signal());
    expect(boot.status).toBe('must_change');
    expect(paths()).toEqual(['GET /api/v1/meta', 'GET /api/v1/session']);
  });

  it('boot opens the workspace once the flag is clear (and treats an absent flag as clear)', async () => {
    for (const body of [{ mustChangePassword: false }, {}]) {
      const { api, paths } = mock((r) => {
        if (r.path === '/api/v1/meta') return res(200, meta);
        if (r.path === '/api/v1/session') return res(200, { user, banner: null, uatSuperadmin: false, ...body });
        if (r.path === '/api/v1/workspaces') return res(200, { items: [] });
        return undefined;
      });
      const boot = await loadBoot(api, signal());
      expect(boot.status).toBe('ready');
      expect(paths()).toContain('GET /api/v1/workspaces');
    }
  });

  it('checks the rules before sending: 12 characters, not the username, not the current one, confirmed', () => {
    const base = { currentPassword: 'Sementara-123456', confirmPassword: '', username: 'Sari' };
    expect(validatePasswordChange({ ...base, newPassword: 'short-11ch!', confirmPassword: 'short-11ch!' }).newPassword)
      .toMatch(/minimal 12/);
    // 12 characters of 2 bytes each still counts as 12 (the server counts characters too).
    expect(validatePasswordChange({ ...base, newPassword: 'ééééééééééé1', confirmPassword: 'ééééééééééé1' })).toEqual({});
    expect(validatePasswordChange({ ...base, username: 'sari.wulandari', newPassword: ' SARI.WULANDARI ', confirmPassword: ' SARI.WULANDARI ' }).newPassword)
      .toMatch(/nama pengguna/);
    expect(validatePasswordChange({ ...base, newPassword: base.currentPassword, confirmPassword: base.currentPassword }).newPassword)
      .toMatch(/berbeda/);
    expect(validatePasswordChange({ ...base, newPassword: 'Baru-dan-panjang', confirmPassword: 'Baru-dan-panjanG' }).confirmPassword)
      .toMatch(/Konfirmasi/);
    expect(validatePasswordChange({ ...base, currentPassword: '', newPassword: 'Baru-dan-panjang', confirmPassword: 'Baru-dan-panjang' }).currentPassword)
      .toBeTruthy();
    // After a reload the username is unknown: the server still enforces that rule.
    expect(validatePasswordChange({ ...base, username: null, newPassword: 'sari-is-long-enough', confirmPassword: 'sari-is-long-enough' })).toEqual({});
  });

  it('posts with the CSRF token and maps a 422 field to the matching input', async () => {
    let answer = res(204);
    const { api, requests } = mock((r) => {
      if (r.path === '/api/v1/login') return issued(true);
      if (r.path === '/api/v1/account/password') return answer;
      return undefined;
    });
    await api.login('sari', 'Sementara-123456');
    await api.changePassword('Sementara-123456', 'Baru-dan-panjang');
    const sent = requests.at(-1)!;
    expect(sent.method).toBe('POST');
    expect(sent.headers['X-CSRF-Token']).toBe('csrf-1');
    expect(sent.body).toEqual({ currentPassword: 'Sementara-123456', newPassword: 'Baru-dan-panjang' });

    answer = err(422, 'validation_failed', { field: 'currentPassword' });
    const wrong = await api.changePassword('salah', 'Baru-dan-panjang').catch((x: unknown) => x);
    expect(passwordChangeError(wrong)).toEqual({ fields: { currentPassword: 'Kata sandi saat ini salah.' }, form: null });

    answer = err(422, 'validation_failed', { field: 'newPassword' });
    const weak = passwordChangeError(await api.changePassword('Sementara-123456', 'sari-sari-sari').catch((x: unknown) => x));
    expect(weak.fields.newPassword).toMatch(/12 karakter.*nama pengguna.*berbeda/);
    expect(weak.form).toBeNull();

    answer = err(429, 'rate_limited', { retryAfterSeconds: 5 });
    const limited = passwordChangeError(await api.changePassword('a', 'b').catch((x: unknown) => x));
    expect(limited).toEqual({ fields: {}, form: expect.stringContaining('5 detik') as string });
  });
});

describe('CP3a logout everywhere (US-1.6)', () => {
  it('posts with CSRF, forgets the token and does not report the end as an expiry', async () => {
    const { api, requests } = mock((r) => {
      if (r.path === '/api/v1/login') return issued(false);
      if (r.path === '/api/v1/session/logout-all') return res(204);
      return undefined;
    });
    let ended = 0;
    api.onSessionEnded(() => { ended += 1; });
    await api.login('sari', 'pw');
    await api.logoutAll();
    const sent = requests.at(-1)!;
    expect(`${sent.method} ${sent.path}`).toBe('POST /api/v1/session/logout-all');
    expect(sent.headers['X-CSRF-Token']).toBe('csrf-1');
    expect(api.hasCsrf()).toBe(false);
    expect(ended).toBe(0);
  });
});

describe('CP3a session ended mid-use', () => {
  it('a 401 after the session was established notifies once and drops the CSRF token', async () => {
    const { api } = mock((r) => {
      if (r.path === '/api/v1/session') return res(200, { user, banner: null, uatSuperadmin: false, mustChangePassword: false });
      if (r.path === '/api/v1/session/csrf') return res(200, { csrfToken: 'csrf-2' });
      if (r.path.startsWith('/api/v1/workspaces')) return err(401, 'unauthenticated');
      return undefined;
    });
    let ended = 0;
    const off = api.onSessionEnded(() => { ended += 1; });
    await api.getSession();
    await expect(api.listWorkspaces()).rejects.toMatchObject({ status: 401 });
    await expect(api.getWorkspace('w-1')).rejects.toMatchObject({ status: 401 });
    expect(ended).toBe(1);
    expect(api.hasCsrf()).toBe(false);
    off();
  });

  it('a 401 step_up_required or mfa_invalid refuses the action but keeps the session (CP4b)', async () => {
    const { api } = mock((r) => {
      if (r.path === '/api/v1/session') return res(200, { user, banner: null, uatSuperadmin: false, mustChangePassword: false });
      if (r.path === '/api/v1/session/csrf') return res(200, { csrfToken: 'csrf-2' });
      if (r.path === '/api/v1/users') return err(401, 'step_up_required');
      if (r.path.includes('/password-reset')) return err(401, 'mfa_invalid');
      return undefined;
    });
    let ended = 0;
    api.onSessionEnded(() => { ended += 1; });
    await api.getSession();
    await expect(api.createUser({ username: 'x', displayName: 'X' } as never)).rejects.toMatchObject({ status: 401, code: 'step_up_required' });
    await expect(api.resetUserPassword('u-1')).rejects.toMatchObject({ status: 401, code: 'mfa_invalid' });
    expect(ended).toBe(0);
    expect(api.hasCsrf()).toBe(true);
  });

  it('a 401 on login or on the first session read is not an expiry', async () => {
    const { api } = mock((r) => {
      if (r.path === '/api/v1/login') return err(401, 'unauthenticated');
      if (r.path === '/api/v1/session') return err(401, 'unauthenticated');
      return undefined;
    });
    let ended = 0;
    api.onSessionEnded(() => { ended += 1; });
    await expect(api.getSession()).rejects.toMatchObject({ status: 401 });
    await expect(api.login('sari', 'salah')).rejects.toMatchObject({ status: 401 });
    expect(ended).toBe(0);
  });

  it('a 401 while refreshing the CSRF token after a reload also ends the session', async () => {
    const { api } = mock((r) => {
      if (r.path === '/api/v1/session') return res(200, { user, banner: null, uatSuperadmin: false });
      if (r.path === '/api/v1/session/csrf') return err(401, 'unauthenticated');
      return undefined;
    });
    let ended = 0;
    api.onSessionEnded(() => { ended += 1; });
    await api.getSession();
    await expect(api.logoutAll()).rejects.toMatchObject({ status: 401 });
    expect(ended).toBe(1);
  });
});
