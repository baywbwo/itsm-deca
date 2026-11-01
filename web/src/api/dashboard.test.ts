import { describe, expect, it } from 'vitest';
import { dashboardKey } from '../app/hooks';
import { ScopedLoader } from '../app/scopedLoader';
import { createApi, type TransportRequest, type TransportResponse } from './client';
import { isDashboard } from './guards';
import type { Dashboard } from './types';

const WS_A = '00000000-0000-4000-8000-00000000000a';
const WS_B = '00000000-0000-4000-8000-00000000000b';

function dashboardFixture(workspaceId = WS_A, key: Dashboard['period']['key'] = 'today'): Dashboard {
  const filter = { status: ['new' as const], owner: 'unassigned' };
  return {
    workspaceId,
    workspaceCode: workspaceId === WS_A ? 'DUMA' : 'DUMB',
    asOf: '2026-11-01T07:00:00.000000Z',
    period: {
      key, basis: 'received', timezone: 'Asia/Jakarta', startDate: '2026-11-01', endDate: '2026-11-01',
      startAt: '2026-10-31T17:00:00.000000Z', endAt: '2026-11-01T17:00:00.000000Z',
    },
    slaConfigured: false,
    totals: {
      tickets: 3, active: 2, unassigned: 1, pendingCustomer: 0, receivedInPeriod: 3, resolvedInPeriod: 1,
      resolvedTicketsInPeriod: 1,
    },
    drilldown: {
      active: filter, unassigned: filter, pendingCustomer: filter, receivedInPeriod: filter,
      resolvedInPeriod: filter, attention: filter,
    },
    statusDistribution: [
      { status: { code: 'new', label: 'Baru', selectable: true, archived: false }, active: true, count: 2, filter },
      { status: { code: 'resolved', label: 'Terselesaikan', selectable: true, archived: false }, active: false, count: 1, filter },
    ],
    picLoad: [{ userId: null, displayName: null, memberActive: true, activeTickets: 2, filter }],
    channels: [{ channelId: null, code: null, label: null, count: 3 }],
    age: [{ key: 'lt_1d', label: '< 1 hari', count: 2 }],
    trend: [{ date: '2026-11-01', received: 3, resolved: 1 }],
    attention: { total: 0, limit: 10, items: [] },
  };
}

function fake(responses: Record<string, TransportResponse>) {
  const seen: TransportRequest[] = [];
  const api = createApi(async (req) => {
    seen.push(req);
    const r = responses[`${req.method} ${req.path}`];
    if (!r) throw new Error(`unexpected ${req.method} ${req.path}`);
    return r;
  });
  return { api, seen };
}

function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}
const flush = () => new Promise((r) => setTimeout(r, 0));

describe('dashboard api', () => {
  it('requests one workspace and period and accepts the contract shape', async () => {
    const { api, seen } = fake({
      [`GET /api/v1/workspaces/${WS_A}/dashboard?period=last_7_days`]: {
        status: 200, body: dashboardFixture(WS_A, 'last_7_days'), requestId: 'r1',
      },
    });
    const d = await api.getDashboard(WS_A, 'last_7_days');
    expect(d.totals.active).toBe(2);
    expect(seen).toHaveLength(1);
  });

  it('rejects a payload that does not match the contract instead of rendering it', async () => {
    const broken = { ...dashboardFixture(), totals: { ...dashboardFixture().totals, active: '2' } };
    expect(isDashboard(broken)).toBe(false);
    expect(isDashboard({ ...dashboardFixture(), period: { ...dashboardFixture().period, key: '7d' } })).toBe(false);
    expect(isDashboard({ ...dashboardFixture(), attention: { total: 0, items: [{}] } })).toBe(false);
    const { api } = fake({
      [`GET /api/v1/workspaces/${WS_A}/dashboard?period=today`]: { status: 200, body: broken, requestId: null },
    });
    await expect(api.getDashboard(WS_A, 'today')).rejects.toMatchObject({ code: 'bad_response' });
  });

  it('maps invalid_request and the 404 of a foreign workspace', async () => {
    const { api } = fake({
      [`GET /api/v1/workspaces/${WS_B}/dashboard?period=today`]: {
        status: 404, body: { error: { code: 'not_found', message: 'Data tidak ditemukan.' } }, requestId: 'r9',
      },
      [`GET /api/v1/workspaces/${WS_A}/dashboard?period=this_month`]: {
        status: 400, body: { error: { code: 'invalid_request', message: 'Parameter permintaan tidak valid.' } }, requestId: null,
      },
    });
    await expect(api.getDashboard(WS_B, 'today')).rejects.toMatchObject({ status: 404, code: 'not_found' });
    await expect(api.getDashboard(WS_A, 'this_month')).rejects.toMatchObject({ status: 400, code: 'invalid_request' });
  });
});

describe('dashboard switch race', () => {
  it('never shows numbers of a previous workspace or period after a switch', async () => {
    const loader = new ScopedLoader<string, Dashboard>();
    const aToday = deferred<Dashboard>();
    const aWeek = deferred<Dashboard>();
    const bWeek = deferred<Dashboard>();
    const signals: AbortSignal[] = [];
    const start = (k: string, p: Promise<Dashboard>) => loader.load(k, (s) => {
      signals.push(s);
      return p;
    });
    start(dashboardKey(WS_A, 'today'), aToday.promise);
    start(dashboardKey(WS_A, 'last_7_days'), aWeek.promise); // period switch
    start(dashboardKey(WS_B, 'last_7_days'), bWeek.promise); // workspace switch
    expect(signals.map((s) => s.aborted)).toEqual([true, true, false]);
    // Late answers of the old keys arrive first and later: both are ignored.
    aWeek.resolve(dashboardFixture(WS_A, 'last_7_days'));
    await flush();
    expect(loader.getState()).toEqual({ status: 'loading', key: dashboardKey(WS_B, 'last_7_days') });
    bWeek.resolve(dashboardFixture(WS_B, 'last_7_days'));
    await flush();
    aToday.resolve(dashboardFixture(WS_A, 'today'));
    await flush();
    const s = loader.getState();
    expect(s.status === 'success' && s.data.workspaceCode).toBe('DUMB');
    expect(s.status === 'success' && s.key).toBe(dashboardKey(WS_B, 'last_7_days'));
  });
});
