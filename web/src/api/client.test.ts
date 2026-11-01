import { describe, expect, it } from 'vitest';
import { ApiError, createApi, type TransportRequest, type TransportResponse } from './client';

function fake(responses: Record<string, TransportResponse>) {
  const seen: TransportRequest[] = [];
  const transport = async (req: TransportRequest) => {
    seen.push(req);
    const r = responses[`${req.method} ${req.path}`];
    if (!r) throw new Error(`unexpected ${req.method} ${req.path}`);
    return r;
  };
  return { seen, api: createApi(transport) };
}

const session = { user: { id: 'u1', displayName: 'Superadmin Uji (dummy)' }, banner: 'Mode uji — Superadmin', uatSuperadmin: true };

describe('api client', () => {
  it('UAT entry sends the entry header and keeps the CSRF token in memory only', async () => {
    const { api, seen } = fake({
      'POST /api/v1/uat/session': { status: 201, body: { ...session, csrfToken: 'tok' }, requestId: 'r1' },
    });
    expect(api.csrfHeaders()).toEqual({});
    const s = await api.enterUat();
    expect(seen[0]?.headers).toEqual({ 'X-ITSM-UAT-Entry': '1' });
    expect(s).toEqual(session);
    expect('csrfToken' in s).toBe(false);
    expect(api.csrfHeaders()).toEqual({ 'X-CSRF-Token': 'tok' });
  });

  it('maps the stable error envelope', async () => {
    const { api } = fake({
      'GET /api/v1/workspaces/x/permissions': {
        status: 404,
        body: { error: { code: 'not_found', message: 'Data tidak ditemukan.' } },
        requestId: 'r2',
      },
    });
    await expect(api.getMyPermissions('x')).rejects.toMatchObject({ status: 404, code: 'not_found', requestId: 'r2' });
  });

  it('rejects payloads that do not match the contract', async () => {
    const { api } = fake({
      'GET /api/v1/workspaces': { status: 200, body: { items: [{ id: 1 }] }, requestId: null },
    });
    const err = await api.listWorkspaces().catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(err).toMatchObject({ code: 'bad_response' });
  });

  it('encodes workspace ids into the path', async () => {
    const { api, seen } = fake({
      'GET /api/v1/workspaces/a%2Fb': { status: 404, body: { error: { code: 'not_found', message: 'x' } }, requestId: null },
    });
    await api.getWorkspace('a/b').catch(() => undefined);
    expect(seen[0]?.path).toBe('/api/v1/workspaces/a%2Fb');
  });
});
