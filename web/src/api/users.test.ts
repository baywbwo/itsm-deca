import { describe, expect, it } from 'vitest';
import { ApiError, createApi, type TransportRequest, type TransportResponse } from './client';
import { isSession, isTemporaryPassword, isUserCreated, isUserList } from './guards';
import { userWriteError } from '../app/users';

// CP3b user administration client: platform paths, CSRF on writes, guards at the boundary.
// Synthetic data only; the temporary password is a placeholder string.
const item = {
  id: 'u-2', username: 'budi.santoso', displayName: 'Budi Santoso', email: 'budi@contoh.test', active: true,
  mustChangePassword: true, locked: false, platformAdmin: false, mfaEnrolled: false, createdAt: '2026-11-12T03:00:00Z', deactivatedAt: null,
};
const TEMP = 'contoh-sandi-sementara-0001';

function harness(answer: (req: TransportRequest) => TransportResponse) {
  const seen: TransportRequest[] = [];
  const api = createApi(async (req) => {
    seen.push(req);
    if (req.path === '/api/v1/login') {
      return { status: 201, requestId: null, body: {
        user: { id: 'u-1', displayName: 'Admin' }, banner: null, uatSuperadmin: false, mustChangePassword: false,
        platformAdmin: true, csrfToken: 'tok',
      } };
    }
    return answer(req);
  });
  return { api, seen };
}

describe('CP3b user administration client', () => {
  it('login keeps platformAdmin from the server; absent reads as false', async () => {
    const { api } = harness(() => ({ status: 204, requestId: null, body: null }));
    expect((await api.login('admin', 'x')).platformAdmin).toBe(true);
    const base = { user: { id: 'u', displayName: 'U' }, banner: null, uatSuperadmin: false };
    expect(isSession(base)).toBe(true);
    expect(isSession({ ...base, platformAdmin: 'yes' })).toBe(false);
  });

  it('lists with trimmed search, active filter and cursor; no CSRF needed for GET', async () => {
    const { api, seen } = harness(() => ({ status: 200, requestId: null, body: { items: [item], nextCursor: 'c2' } }));
    const page = await api.listUsers({ q: '  budi ', active: 'false' }, 'c1');
    expect(page.items[0]?.username).toBe('budi.santoso');
    expect(seen[0]?.method).toBe('GET');
    expect(seen[0]?.path).toBe('/api/v1/users?q=budi&active=false&cursor=c1');
    await api.listUsers({ q: '   ', active: 'all' }, null);
    expect(seen[1]?.path).toBe('/api/v1/users?active=all');
  });

  it('create, reset and deactivate send the CSRF token to the platform paths', async () => {
    const { api, seen } = harness((req) => {
      if (req.method === 'POST' && req.path === '/api/v1/users') {
        return { status: 201, requestId: null, body: { user: { id: 'u-3', username: 'citra', displayName: 'Citra', email: null, active: true, mustChangePassword: true }, temporaryPassword: TEMP } };
      }
      if (req.path.endsWith('/password-reset')) return { status: 200, requestId: null, body: { temporaryPassword: TEMP } };
      return { status: 200, requestId: null, body: { id: 'u-3', active: false } };
    });
    await api.login('admin', 'x');
    const created = await api.createUser({ username: 'citra', displayName: 'Citra', email: null });
    expect(created.temporaryPassword).toBe(TEMP);
    expect((await api.resetUserPassword('u/3')).temporaryPassword).toBe(TEMP);
    expect((await api.deactivateUser('u-3')).active).toBe(false);
    const writes = seen.slice(1);
    expect(writes.map((r) => `${r.method} ${r.path}`)).toEqual([
      'POST /api/v1/users', 'POST /api/v1/users/u%2F3/password-reset', 'PATCH /api/v1/users/u-3',
    ]);
    expect(writes.every((r) => r.headers['X-CSRF-Token'] === 'tok')).toBe(true);
    expect(writes[2]?.body).toEqual({ active: false });
  });

  it('guards refuse a missing or empty temporary password and malformed list rows', () => {
    expect(isTemporaryPassword({ temporaryPassword: '' })).toBe(false);
    expect(isTemporaryPassword({})).toBe(false);
    expect(isUserCreated({ user: { id: 'u', username: 'a', displayName: 'A', active: true, mustChangePassword: true } })).toBe(false);
    expect(isUserList({ items: [item], nextCursor: null })).toBe(true);
    expect(isUserList({ items: [{ ...item, locked: undefined }], nextCursor: null })).toBe(false);
    expect(isUserList({ items: [{ ...item, mfaEnrolled: undefined }], nextCursor: null })).toBe(false);
    // The list never carries secrets; an unexpected extra field is ignored, not shown.
    expect(Object.keys(item)).not.toContain('passwordHash');
  });

  it('maps refusals to clear words: field conflicts, last admin, already inactive', () => {
    const err = (status: number, code: string, field: string | null = null) =>
      new ApiError(status, code as never, 'x', 'req-1', field);
    expect(userWriteError(err(409, 'username_taken'), 'create')).toMatchObject({ field: 'username', text: 'Nama pengguna sudah dipakai akun lain.' });
    expect(userWriteError(err(409, 'email_taken'), 'create').field).toBe('email');
    expect(userWriteError(err(422, 'validation_failed', 'displayName'), 'create').field).toBe('displayName');
    const last = userWriteError(err(409, 'last_admin'), 'deactivate');
    expect(last.text).toContain('admin platform terakhir');
    expect(last.reload).toBe(false);
    const inactive = userWriteError(err(409, 'user_inactive'), 'reset');
    expect(inactive.text).toContain('sudah nonaktif');
    expect(inactive.reload).toBe(true);
    expect(userWriteError(err(403, 'forbidden'), 'deactivate').text).toContain('Hanya admin platform');
    expect(userWriteError(err(404, 'not_found'), 'reset').reload).toBe(true);
    expect(userWriteError(err(409, 'last_admin'), 'deactivate').text).toContain('(ID permintaan req-1)');
  });
});
