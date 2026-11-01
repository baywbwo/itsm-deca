import { describe, expect, it } from 'vitest';
import { ApiError, createApi, type TransportRequest, type TransportResponse } from './client';

function fake(responses: Record<string, TransportResponse>) {
  const seen: TransportRequest[] = [];
  const transport = async (req: TransportRequest) => {
    // D11: a client that never entered re-issues its CSRF token first (kept out of `seen`).
    if (req.path === '/api/v1/session/csrf' && !responses['POST /api/v1/session/csrf']) {
      return { status: 200, body: { csrfToken: 're-issued' }, requestId: null };
    }
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

const status = (code: string, label: string) => ({ code, label, selectable: true, archived: code === 'closed' });
const item = {
  id: 't1', number: 7, displayNumber: 'DUMA-7', subject: 'Dana belum masuk', status: status('open', 'Ditangani'),
  cycleNo: 1, version: 3, priority: null, owner: { id: 'u2', label: 'CS Alfa (dummy)' }, channel: null, category: null,
  receivedAt: '2026-11-01T01:00:00Z', updatedAt: '2026-11-01T02:00:00Z', ageSeconds: 3600,
  relatedWork: { open: 0, blockingOpen: 0 },
  allowedActions: [{ to: status('resolved', 'Terselesaikan'), kind: 'resolve', requires: 'summary', blockedBy: null }],
};

describe('ticket endpoints', () => {
  it('list keeps the canonical query and appends the cursor', async () => {
    const page = { items: [item], nextCursor: null, total: 1, countsByStatus: [], resolutionEvents: null,
      basis: { unit: 'ticket', dimensions: 'current_ticket_attributes', resolved: null },
      filters: {}, asOf: '2026-11-01T03:00:00Z' };
    const { api, seen } = fake({
      'GET /api/v1/workspaces/w/tickets?status=open&cursor=c1': { status: 200, body: page, requestId: null },
    });
    const res = await api.listTickets('w', 'status=open', 'c1');
    expect(seen[0]?.path).toBe('/api/v1/workspaces/w/tickets?status=open&cursor=c1');
    expect(res.items.map((t) => t.displayNumber)).toEqual(['DUMA-7']);
    expect(res.items[0]?.allowedActions[0]?.requires).toBe('summary');
  });

  it('transition POSTs JSON with the CSRF header and returns the server ticket', async () => {
    const { api, seen } = fake({
      'POST /api/v1/uat/session': { status: 201, body: { ...session, csrfToken: 'tok' }, requestId: null },
      'POST /api/v1/workspaces/w/tickets/t1/transitions': {
        status: 200, body: { ticket: { ...item, version: 4 }, auditEventId: 'a1' }, requestId: 'r9',
      },
    });
    await api.enterUat();
    const body = { to: 'resolved' as const, version: 3, summary: 'Dana dikembalikan' };
    const res = await api.transitionTicket('w', 't1', body);
    expect(seen[1]).toMatchObject({ method: 'POST', headers: { 'X-CSRF-Token': 'tok' }, body });
    expect(res.ticket.version).toBe(4);
  });

  it('a version conflict surfaces as 409 without a ticket', async () => {
    const { api } = fake({
      'POST /api/v1/workspaces/w/tickets/t1/assignment': {
        status: 409, body: { error: { code: 'version_conflict', message: 'x' } }, requestId: 'r3',
      },
    });
    await expect(api.assignTicket('w', 't1', { ownerUserId: null, version: 1 }))
      .rejects.toMatchObject({ status: 409, code: 'version_conflict' });
  });

  it('a 200 mutation with a bad shape is not trusted', async () => {
    const { api } = fake({
      'POST /api/v1/workspaces/w/tickets/t1/transitions': {
        status: 200, body: { ticket: { ...item, allowedActions: [{ to: 'x' }] }, auditEventId: 'a' }, requestId: null,
      },
    });
    await expect(api.transitionTicket('w', 't1', { to: 'open', version: 1 })).rejects.toMatchObject({ code: 'bad_response' });
  });
});

describe('saved views client (C32)', () => {
  const view = { id: 'v1', name: 'Email P1', view: 'board', query: { channel: 'c1', sort: 'received', order: 'desc' }, columns: ['status', 'owner'], version: 1, createdAt: 't', updatedAt: 't' };
  const base = '/api/v1/workspaces/w/saved-views';

  it('creates with CSRF (201), renames with version, deletes with 204', async () => {
    const { api, seen } = fake({
      'POST /api/v1/uat/session': { status: 201, body: { ...session, csrfToken: 'tok' }, requestId: 'r0' },
      [`POST ${base}`]: { status: 201, body: view, requestId: 'r1' },
      [`PATCH ${base}/v1`]: { status: 200, body: { ...view, name: 'Baru', version: 2 }, requestId: 'r2' },
      [`DELETE ${base}/v1`]: { status: 204, body: null, requestId: 'r3' },
      [`GET ${base}`]: { status: 200, body: { items: [view] }, requestId: 'r4' },
    });
    await api.enterUat();
    expect(await api.createSavedView('w', { name: 'Email P1', view: 'board', query: view.query })).toEqual(view);
    expect(seen[1]?.headers).toMatchObject({ 'X-CSRF-Token': 'tok' });
    expect((await api.renameSavedView('w', 'v1', { name: 'Baru', version: 1 })).version).toBe(2);
    expect(seen[2]?.method).toBe('PATCH');
    expect(seen[2]?.headers).toMatchObject({ 'X-CSRF-Token': 'tok' });
    await expect(api.deleteSavedView('w', 'v1')).resolves.toBeUndefined();
    expect(seen[3]?.headers).toMatchObject({ 'X-CSRF-Token': 'tok' });
    expect(await api.listSavedViews('w')).toEqual([view]);
  });

  it('rejects a stored view that carries search text or non-string values', async () => {
    const { api } = fake({
      [`GET ${base}`]: { status: 200, body: { items: [{ ...view, query: { q: 'nama orang' } }] }, requestId: 'r' },
      [`POST ${base}`]: { status: 201, body: { ...view, query: { status: 1 } }, requestId: 'r' },
    });
    await expect(api.listSavedViews('w')).rejects.toMatchObject({ code: 'bad_response' });
    await expect(api.createSavedView('w', { name: 'x', view: 'list', query: {} })).rejects.toMatchObject({ code: 'bad_response' });
  });
});

describe('D08b follow-up client', () => {
  it('upload sends the raw file with CSRF, an encoded file name and the visibility; errors keep their code', async () => {
    const path = '/api/v1/workspaces/w/tickets/t/attachments?visibility=public';
    const { api, seen } = fake({
      [`POST ${path}`]: { status: 503, body: { error: { code: 'scanner_unavailable', message: 'x' } }, requestId: 'r' },
    });
    const file = new File(['%PDF-1.4'], 'laporan akhir/ä.pdf', { type: 'application/pdf' });
    await expect(api.uploadTicketAttachment('w', 't', file, 'public')).rejects.toMatchObject({ status: 503, code: 'scanner_unavailable' });
    expect(seen[0]?.file).toBe(file);
    expect(seen[0]?.body).toBeUndefined();
    expect(seen[0]?.headers['X-File-Name']).toBe(encodeURIComponent('laporan akhir/ä.pdf'));
  });

  it('notes and related work carry the Idempotency-Key; a note without an author is not trusted', async () => {
    const path = '/api/v1/workspaces/w/tickets/t/notes';
    const note = { id: 'n', body: 'b', author: null, createdAt: '2026-11-01T00:00:00Z' };
    const { api, seen } = fake({ [`POST ${path}`]: { status: 201, body: { note, replayed: false, auditEventId: 'a' }, requestId: null } });
    await expect(api.createTicketNote('w', 't', 'k', { body: 'b' })).rejects.toMatchObject({ code: 'bad_response' });
    expect(seen[0]?.headers['Idempotency-Key']).toBe('k');
  });

  it('download link stays same-origin and encoded', () => {
    const { api } = fake({});
    expect(api.attachmentContentUrl('w', 't', 'a/b')).toBe('/api/v1/workspaces/w/tickets/t/attachments/a%2Fb/content');
  });
});

describe('D11 CSRF re-issue after a reload', () => {
  const view = { name: 'v', filters: {} };
  const err = (code: string) => ({ status: 403, body: { error: { code, message: 'x' } }, requestId: null });
  function scripted(answers: Array<(req: TransportRequest) => TransportResponse>) {
    const seen: TransportRequest[] = [];
    const api = createApi(async (req) => {
      seen.push(req);
      const next = answers.shift();
      if (!next) throw new Error(`unexpected ${req.method} ${req.path}`);
      return next(req);
    });
    return { api, seen };
  }
  const issued = (t: string) => () => ({ status: 200, body: { csrfToken: t }, requestId: null });

  it('a restored session gets a token from the refresh endpoint before its first unsafe request', async () => {
    const { api, seen } = scripted([issued('t1'), () => err('not_found')]);
    await api.createSavedView('w1', view as never).catch(() => undefined);
    expect(seen.map((r) => `${r.method} ${r.path}`)).toEqual(['POST /api/v1/session/csrf', 'POST /api/v1/workspaces/w1/saved-views']);
    expect(seen[0]?.headers).toEqual({ 'X-ITSM-CSRF-Refresh': '1' });
    expect(seen[1]?.headers['X-CSRF-Token']).toBe('t1');
    expect(api.csrfHeaders()).toEqual({ 'X-CSRF-Token': 't1' });
  });

  it('GET never refreshes; a csrf_failed refusal refreshes and retries exactly once', async () => {
    const { api, seen } = scripted([
      () => ({ status: 200, body: { items: [] }, requestId: null }),
      issued('t1'), () => err('csrf_failed'), issued('t2'), () => err('csrf_failed'),
    ]);
    await api.listWorkspaces();
    const e = await api.createSavedView('w1', view as never).catch((x: unknown) => x);
    expect(e).toMatchObject({ status: 403, code: 'csrf_failed' });
    expect(seen.map((r) => r.headers['X-CSRF-Token'] ?? r.headers['X-ITSM-CSRF-Refresh'] ?? '-')).toEqual(['-', '1', 't1', '1', 't2']);
  });

  it('a forbidden (permission) answer is not retried', async () => {
    const { api, seen } = scripted([issued('t1'), () => err('forbidden')]);
    await expect(api.createSavedView('w1', view as never)).rejects.toMatchObject({ code: 'forbidden' });
    expect(seen).toHaveLength(2);
  });

  it('concurrent unsafe requests share one refresh', async () => {
    let release!: () => void;
    const gate = new Promise<void>((r) => { release = r; });
    const seen: string[] = [];
    const api = createApi(async (req) => {
      seen.push(req.path);
      if (req.path === '/api/v1/session/csrf') { await gate; return issued('t1')(); }
      return err('not_found');
    });
    const a = api.createSavedView('w1', view as never).catch(() => undefined);
    const b = api.createSavedView('w2', view as never).catch(() => undefined);
    release(); await Promise.all([a, b]);
    expect(seen.filter((p) => p === '/api/v1/session/csrf')).toHaveLength(1);
  });

  it('a failed refresh surfaces its error (401 stays a session problem) and is not cached', async () => {
    const { api, seen } = scripted([
      () => ({ status: 401, body: { error: { code: 'unauthenticated', message: 'x' } }, requestId: null }),
      issued('t1'), () => err('not_found'),
    ]);
    await expect(api.createSavedView('w1', view as never)).rejects.toMatchObject({ status: 401, code: 'unauthenticated' });
    await api.createSavedView('w1', view as never).catch(() => undefined);
    expect(seen.map((r) => r.path)).toEqual(['/api/v1/session/csrf', '/api/v1/session/csrf', '/api/v1/workspaces/w1/saved-views']);
  });
});
