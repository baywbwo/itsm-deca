import { describe, expect, it } from 'vitest';
import {
  NEW_TICKET_FLAG,
  PAGES,
  allowedPages,
  dashboardQuery,
  drilldownHash,
  filterToQuery,
  hasDashboardFilter,
  navGroups,
  parseDashboardQuery,
  parseHash,
  toHash,
} from './routes';

describe('routes', () => {
  it('round-trips workspace and page through the hash', () => {
    const id = '7f1c2a8e-0000-4000-8000-000000000001';
    expect(parseHash(toHash({ workspaceId: id, page: 'tickets' }))).toEqual({ workspaceId: id, page: 'tickets', query: '' });
  });

  it('falls back safely on unknown input', () => {
    expect(parseHash('')).toEqual({ workspaceId: null, page: 'dashboard', query: '' });
    expect(parseHash('#/x/y')).toEqual({ workspaceId: null, page: 'dashboard', query: '' });
    expect(parseHash('#/w/abc/unknown')).toEqual({ workspaceId: 'abc', page: 'dashboard', query: '' });
    // Old deep link to the removed Komponen page lands on the dashboard, never the showcase.
    expect(parseHash('#/w/abc/components')).toEqual({ workspaceId: 'abc', page: 'dashboard', query: '' });
    expect(parseHash('#/x?w=1')).toEqual({ workspaceId: null, page: 'dashboard', query: '' });
  });

  it('derives navigation only from server permissions (default deny)', () => {
    expect(allowedPages([], false).map((p) => p.id)).toEqual([]);
    // D13: the UAT-only component showcase is gone; UAT grants no extra page.
    expect(allowedPages([], true).map((p) => p.id)).toEqual([]);
    expect(PAGES.map((p) => p.id)).not.toContain('components');
    const cs = ['member.read', 'catalog.read', 'ticket.read', 'report.view'] as const;
    expect(allowedPages(cs, false).map((p) => p.id)).toEqual(['dashboard', 'tickets', 'reports', 'settings']);
    expect(allowedPages(['integration.recovery'], false).map((p) => p.id)).toEqual(['integrations']);
  });

  it('CP3b: Pengguna only for a platform admin session, never from workspace permissions or UAT', () => {
    const all = PAGES.flatMap((p) => p.anyOf);
    expect(allowedPages(all, true).map((p) => p.id)).not.toContain('users');
    expect(allowedPages([], false, true).map((p) => p.id)).toEqual(['users']);
    expect(allowedPages(['admin.settings'], false, true).map((p) => p.id)).toEqual(['settings', 'users']);
    // All three sit in the Administrasi nav section.
    expect(PAGES.filter((p) => p.admin).map((p) => p.id)).toEqual(['settings', 'integrations', 'users']);
    expect(parseHash('#/w/abc/users')).toEqual({ workspaceId: 'abc', page: 'users', query: '' });
  });

  it('carries drilldown filters in the hash query without touching the path', () => {
    const id = '7f1c2a8e-0000-4000-8000-000000000001';
    const hash = drilldownHash(id, {
      status: ['new', 'open', 'pending_customer', 'pending_engineering'],
      owner: 'unassigned',
      receivedFrom: '2026-10-31T17:00:00.000000Z',
    });
    expect(hash).toBe(
      `#/w/${id}/tickets?status=new%2Copen%2Cpending_customer%2Cpending_engineering&owner=unassigned` +
        '&receivedFrom=2026-10-31T17%3A00%3A00.000000Z',
    );
    const r = parseHash(hash);
    expect(r).toMatchObject({ workspaceId: id, page: 'tickets' });
    const q = new URLSearchParams(r.query);
    expect(q.get('status')?.split(',')).toEqual(['new', 'open', 'pending_customer', 'pending_engineering']);
    expect(q.get('owner')).toBe('unassigned');
    expect(q.get('receivedFrom')).toBe('2026-10-31T17:00:00.000000Z');
    expect(filterToQuery({})).toBe('');
    expect(toHash({ workspaceId: id, page: 'tickets', query: '' })).toBe(`#/w/${id}/tickets`);
  });

  it('keeps dashboard period and filters in a canonical URL query', () => {
    const id = '7f1c2a8e-0000-4000-8000-000000000001';
    const ch = '7f1c2a8e-0000-4000-8000-0000000000c1';
    // Order and empty values in a hand-written link do not matter; unknown period falls back.
    const p = parseDashboardQuery(`priority=none&owner=unassigned&channel=${ch}&category=&period=this_month`);
    expect(p).toEqual({
      period: 'this_month',
      filters: { owner: 'unassigned', channel: ch, category: null, priority: 'none' },
    });
    expect(dashboardQuery(p)).toBe(`period=this_month&owner=unassigned&channel=${ch}&priority=none`);
    expect(hasDashboardFilter(p)).toBe(true);
    // Reload: the hash parses back to the same params.
    const hash = toHash({ workspaceId: id, page: 'dashboard', query: dashboardQuery(p) });
    expect(parseDashboardQuery(parseHash(hash).query)).toEqual(p);
    const none = parseDashboardQuery('period=7d');
    expect(none.period).toBe('today');
    expect(hasDashboardFilter(none)).toBe(false);
    expect(dashboardQuery(none)).toBe('period=today');
  });

  it('carries channel and category in drilldowns', () => {
    const q = new URLSearchParams(filterToQuery({ channel: 'none', category: 'c1', priority: ['p1', 'p2'] }));
    expect([q.get('channel'), q.get('category'), q.get('priority')]).toEqual(['none', 'c1', 'p1,p2']);
  });
});

describe('CP-UI1 sidebar groups (owner prototype)', () => {
  const ws = 'w-1';
  const route = (page: 'dashboard' | 'tickets' | 'reports' | 'users', query = '') => ({ workspaceId: ws, page, query });
  const agent = allowedPages(['ticket.read', 'ticket.create', 'report.view'], false, false);
  const admin = allowedPages(['ticket.read', 'ticket.create', 'report.view', 'admin.settings', 'integration.manage'], false, true);
  const shape = (g: ReturnType<typeof navGroups>) => g.map((x) => [x.label, x.items.map((i) => i.label)]);

  it('Operasional / Analitik / Administrasi; Pengguna only for a platform admin', () => {
    expect(shape(navGroups(agent, route('dashboard'), true))).toEqual([
      ['Operasional', ['Dashboard', 'Tiket', 'Board', 'Tiket baru']],
      ['Analitik', ['Laporan']],
    ]);
    expect(shape(navGroups(admin, route('dashboard'), true))).toEqual([
      ['Operasional', ['Dashboard', 'Tiket', 'Board', 'Tiket baru']],
      ['Analitik', ['Laporan']],
      ['Administrasi', ['Pengaturan', 'Integrasi', 'Pengguna']],
    ]);
  });

  it('Board and Tiket baru are the Tiket page (no new route); Tiket baru needs ticket.create', () => {
    const ops = navGroups(agent, route('dashboard'), true)[0]!.items;
    expect(ops.find((i) => i.key === 'board')!.href).toBe(toHash({ workspaceId: ws, page: 'tickets', query: 'view=board' }));
    expect(ops.find((i) => i.key === 'new')!.href).toBe(toHash({ workspaceId: ws, page: 'tickets', query: `${NEW_TICKET_FLAG}=1` }));
    expect(navGroups(agent, route('dashboard'), false)[0]!.items.map((i) => i.key)).not.toContain('new');
  });

  it('exactly one current item: Board in board view, Tiket in list view', () => {
    const current = (r: ReturnType<typeof route>) => navGroups(admin, r, true).flatMap((g) => g.items).filter((i) => i.current).map((i) => i.key);
    expect(current(route('tickets', 'view=board&status=open'))).toEqual(['board']);
    expect(current(route('tickets', 'status=open'))).toEqual(['tickets']);
    expect(current(route('users'))).toEqual(['users']);
  });

  it('default deny: no ticket permission leaves no Operasional group; no workspace gives no nav', () => {
    expect(navGroups(allowedPages(['report.view'], false), route('reports'), true).map((g) => g.label)).toEqual(['Analitik']);
    expect(navGroups(agent, { workspaceId: null, page: 'dashboard', query: '' }, true)).toEqual([]);
  });
});
