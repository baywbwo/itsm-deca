import { describe, expect, it } from 'vitest';
import { writeError } from '../pages/adminKit';
import { ApiError, createApi, type TransportRequest, type TransportResponse } from './client';
import { isCompanyResult, isCompanyUpdateResult, isIntegrationList, isMemberList, isSettingsBody } from './guards';

function fake(responses: Record<string, TransportResponse>) {
  const seen: TransportRequest[] = [];
  const api = createApi(async (req) => {
    // D11: a client that never entered re-issues its CSRF token first (kept out of `seen`).
    if (req.path === '/api/v1/session/csrf' && !responses['POST /api/v1/session/csrf']) {
      return { status: 200, body: { csrfToken: 're-issued' }, requestId: null };
    }
    seen.push(req);
    const r = responses[`${req.method} ${req.path}`];
    if (!r) throw new Error(`unexpected ${req.method} ${req.path}`);
    return r;
  });
  return { seen, api };
}

const W = '/api/v1/workspaces/w1';
const settings = {
  workspaceId: 'w1', workspaceCode: 'DUMA', workspaceName: 'Dummy A', timezone: 'Asia/Jakarta', companyId: 'c1',
  companyCode: 'DUMA', companyName: 'PT Dummy A', reporterCode: null, active: true, version: 3, memberCount: 2,
  brandKey: null, brandName: null, previousCodes: [],
};
const member = {
  membershipId: 'm1', userId: 'u1', displayName: 'CS Alfa (dummy)', role: 'cs', active: true, version: 1, grants: [],
};
const connection = {
  id: 'i1', provider: 'jira', displayName: 'DUMA Jira', mode: 'simulation', state: 'tested', testedAt: '2026-11-02T01:00:00Z',
  lastTestNote: 'Uji simulasi', simulatedOutcome: 'success', version: 4, directoryCount: 2,
  lastTestOk: true, credential: { state: 'missing', updatedAt: null },
  webhook: { state: 'missing', updatedAt: null, path: '/api/v1/integrations/jira/i1/webhook', url: null },
  settings: { baseUrl: null, email: null, projectKey: null, issueTypeId: null, appId: null, priorityMap: {}, statusMap: {} },
  recent: [{ action: 'integration.test', outcome: 'success', occurredAt: '2026-11-02T01:00:00Z', actor: 'Superadmin' }],
};

describe('D09 api client', () => {
  it('sends a versioned settings patch with the CSRF token', async () => {
    const { api, seen } = fake({
      'POST /api/v1/uat/session': {
        status: 201, requestId: null,
        body: { user: { id: 'u', displayName: 'S' }, banner: 'b', uatSuperadmin: true, csrfToken: 'tok' },
      },
      [`PATCH ${W}/settings`]: { status: 200, body: { settings: { ...settings, version: 4 }, auditEventId: 'a1' }, requestId: null },
    });
    await api.enterUat();
    const r = await api.updateSettings('w1', { workspaceName: 'X', companyName: 'Y', timezone: 'Asia/Jakarta', reporterCode: null, version: 3 });
    expect(r.settings.version).toBe(4);
    expect(seen[1]).toMatchObject({ method: 'PATCH', headers: { 'X-CSRF-Token': 'tok' }, body: { version: 3 } });
  });

  it('maps a stale version to a 409 conflict that keeps the input', async () => {
    const { api } = fake({
      [`PATCH ${W}/members/m1`]: {
        status: 409, requestId: 'r9', body: { error: { code: 'version_conflict', message: 'Data sudah berubah.' } },
      },
    });
    const e = await api.updateMember('w1', 'm1', { role: 'lead_cs', active: true, version: 1 }).catch((x: unknown) => x);
    expect(e).toMatchObject({ status: 409 });
    expect(writeError(e, 'Keanggotaan')).toMatchObject({ conflict: true, title: 'Data sudah diubah pihak lain' });
  });

  it('carries the 422 field so the form can mark it', async () => {
    const { api } = fake({
      [`POST ${W}/catalogs/categories`]: {
        status: 422, requestId: null,
        body: { error: { code: 'validation_failed', message: 'Kode sudah dipakai.', field: 'code' } },
      },
    });
    const e = await api.createCatalogEntry('w1', 'categories', { code: 'dup', label: 'Dup', sortOrder: 1 }).catch((x: unknown) => x);
    expect(writeError(e, 'Kategori')).toMatchObject({ field: 'code', conflict: false });
  });

  it('hits the lifecycle step path and 201 for grants', async () => {
    const { api, seen } = fake({
      [`POST ${W}/integrations/i1/activate`]: { status: 200, body: { item: { ...connection, state: 'active' }, auditEventId: 'a' }, requestId: null },
      [`POST ${W}/members/m1/grants`]: { status: 201, body: { item: { ...member, grants: ['admin.settings'] }, auditEventId: 'a' }, requestId: null },
    });
    expect((await api.stepIntegration('w1', 'i1', 'activate', { version: 4 })).item.state).toBe('active');
    expect((await api.addMemberGrant('w1', 'm1', { grantCode: 'admin.settings', version: 1 })).item.grants).toEqual(['admin.settings']);
    expect(seen.map((s) => s.path)).toEqual([`${W}/integrations/i1/activate`, `${W}/members/m1/grants`]);
  });

  it('D14/D17 edits a company profile on its own path, with the (editable) code', async () => {
    const { api, seen } = fake({
      'PATCH /api/v1/companies/c1/profile': {
        status: 200, requestId: null, body: { item: { ...settings, companyName: 'PT Baru', version: 4 }, auditEventId: 'a1' },
      },
    });
    const body = { code: 'DUMA', companyName: 'PT Baru', workspaceName: 'Dummy A', timezone: 'Asia/Jakarta' as const, reporterCode: null,
      brandKey: null, brandName: null, version: 3 };
    const r = await api.updateCompany('c1', body);
    expect(r.item).toMatchObject({ companyName: 'PT Baru', workspaceCode: 'DUMA', version: 4 });
    expect(seen[0]).toMatchObject({ method: 'PATCH', path: '/api/v1/companies/c1/profile', body });
    expect(seen[0]?.body).toHaveProperty('code', 'DUMA');
  });

  it('D15 sends the branding preset with the profile and rejects anything but a preset', async () => {
    const { api, seen } = fake({
      'PATCH /api/v1/companies/c1/profile': {
        status: 200, requestId: null, body: { item: { ...settings, brandKey: 'ajr', brandName: 'Bravo uji', version: 4 }, auditEventId: 'a1' },
      },
      'PATCH /api/v1/companies/c2/profile': {
        status: 200, requestId: null, body: { item: { ...settings, brandKey: 'https://example.com/x.png' }, auditEventId: 'a2' },
      },
    });
    const body = { code: 'DUMA', companyName: 'PT Dummy A', workspaceName: 'Dummy A', timezone: 'Asia/Jakarta' as const, reporterCode: null,
      brandKey: 'ajr' as const, brandName: 'Bravo uji', version: 3 };
    expect((await api.updateCompany('c1', body)).item).toMatchObject({ brandKey: 'ajr', brandName: 'Bravo uji' });
    expect(seen[0]?.body).toMatchObject({ brandKey: 'ajr', brandName: 'Bravo uji' });
    await expect(api.updateCompany('c2', body)).rejects.toBeInstanceOf(Error);
  });

  it('D14 an unchanged company save carries a null audit id; a stale one is a 409 conflict', async () => {
    const { api } = fake({
      'PATCH /api/v1/companies/c1/profile': { status: 200, requestId: null, body: { item: settings, auditEventId: null } },
      'PATCH /api/v1/companies/c2/profile': {
        status: 409, requestId: 'r1', body: { error: { code: 'version_conflict', message: 'Data sudah berubah.' } },
      },
    });
    const body = { code: 'DUMA', companyName: 'PT Dummy A', workspaceName: 'Dummy A', timezone: 'Asia/Jakarta' as const, reporterCode: null,
      brandKey: null, brandName: null, version: 3 };
    expect((await api.updateCompany('c1', body)).auditEventId).toBeNull();
    const e = await api.updateCompany('c2', body).catch((x: unknown) => x);
    expect(writeError(e, 'Perusahaan')).toMatchObject({ conflict: true, title: 'Data sudah diubah pihak lain' });
  });

  it('D17 renames the code, lists earlier codes and rejects a settings body without them', async () => {
    const { api, seen } = fake({
      'PATCH /api/v1/companies/c1/profile': {
        status: 200, requestId: null,
        body: { item: { ...settings, workspaceCode: 'DECA', companyCode: 'DECA', previousCodes: ['DUMA'], version: 4 }, auditEventId: 'a1' },
      },
      'PATCH /api/v1/companies/c2/profile': {
        status: 200, requestId: null, body: { item: { ...settings, previousCodes: undefined }, auditEventId: 'a2' },
      },
      'PATCH /api/v1/companies/c3/profile': {
        status: 422, requestId: null,
        body: { error: { code: 'validation_failed', message: 'Kode sudah dipakai.', field: 'code' } },
      },
    });
    const body = { code: 'DECA', companyName: 'PT Dummy A', workspaceName: 'Dummy A', timezone: 'Asia/Jakarta' as const,
      reporterCode: null, brandKey: null, brandName: null, version: 3 };
    const r = await api.updateCompany('c1', body);
    expect(r.item).toMatchObject({ workspaceCode: 'DECA', previousCodes: ['DUMA'], reporterCode: null });
    expect(seen[0]?.body).toMatchObject({ code: 'DECA', reporterCode: null });
    await expect(api.updateCompany('c2', body)).rejects.toBeInstanceOf(Error);
    const e = await api.updateCompany('c3', body).catch((x: unknown) => x);
    expect(writeError(e, 'Perusahaan')).toMatchObject({ field: 'code', conflict: false });
  });

  it('403 on companies stays a forbidden error (no silent empty list)', async () => {
    const { api } = fake({
      'GET /api/v1/companies': { status: 403, requestId: null, body: { error: { code: 'forbidden', message: 'Tidak diizinkan.' } } },
    });
    await expect(api.listCompanies()).rejects.toBeInstanceOf(ApiError);
  });
});

const LIST_EXTRA = { integrationsMode: 'simulation', keyringConfigured: false, priorities: [{ code: 'urgent', label: 'Mendesak' }], ticketStatuses: ['new'] };

describe('D09 guards', () => {
  it('accept the contract shapes', () => {
    expect(isSettingsBody({ settings, canEdit: false, timezones: ['Asia/Jakarta'] })).toBe(true);
    expect(isMemberList({ items: [member], myMembershipId: null, canManage: false, grantable: [] })).toBe(true);
    expect(isIntegrationList({ workspaceId: 'w1', items: [connection], ...LIST_EXTRA, creatable: ['lark'], canManageCredentials: false })).toBe(true);
    expect(isCompanyUpdateResult({ item: settings, auditEventId: null })).toBe(true);
    expect(isCompanyResult({ item: settings, auditEventId: null })).toBe(false);
  });

  it('reject unknown grants, timezones, providers and a credential claim', () => {
    expect(isMemberList({ items: [{ ...member, grants: ['superuser'] }], myMembershipId: null, canManage: true, grantable: [] })).toBe(false);
    expect(isSettingsBody({ settings, canEdit: true, timezones: ['Europe/Paris'] })).toBe(false);
    expect(isIntegrationList({ workspaceId: 'w1', items: [{ ...connection, provider: 'whatsapp' }], ...LIST_EXTRA, creatable: [], canManageCredentials: false })).toBe(false);
    expect(isIntegrationList({ workspaceId: 'w1', items: [{ ...connection, credential: 'configured' }], ...LIST_EXTRA, creatable: [], canManageCredentials: true })).toBe(false);
    expect(isIntegrationList({ workspaceId: 'w1', items: [{ ...connection, mode: 'production' }], ...LIST_EXTRA, creatable: [], canManageCredentials: true })).toBe(false);
  });
});
