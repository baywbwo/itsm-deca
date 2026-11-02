import { describe, expect, it } from 'vitest';
import { createApi, type TransportRequest } from './client';
describe('password login bootstrap', () => {
  it('uses its own cross-site protection before a session exists, then uses issued CSRF for logout', async () => {
    const requests: TransportRequest[] = [];
    const api = createApi(async req => {
      requests.push(req);
      if (req.path === '/api/v1/login') return { status: 201, requestId: null, body: {
        user: { id: 'admin', displayName: 'Administrator' }, banner: null, uatSuperadmin: false, csrfToken: 'issued-token',
      } };
      return { status: 204, requestId: null, body: null };
    });
    const session = await api.login('admin@example.test', 'synthetic-password');
    expect(session.uatSuperadmin).toBe(false);
    expect(requests).toHaveLength(1);
    expect(requests[0]?.path).toBe('/api/v1/login');
    expect(requests[0]?.headers['X-ITSM-Login']).toBe('1');
    await api.logout();
    expect(requests[1]?.headers['X-CSRF-Token']).toBe('issued-token');
    expect(api.hasCsrf()).toBe(false);
  });
});
