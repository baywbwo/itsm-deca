import { describe, expect, it } from 'vitest';
import { allowedPages, drilldownHash, filterToQuery, parseHash, toHash } from './routes';

describe('routes', () => {
  it('round-trips workspace and page through the hash', () => {
    const id = '7f1c2a8e-0000-4000-8000-000000000001';
    expect(parseHash(toHash({ workspaceId: id, page: 'tickets' }))).toEqual({ workspaceId: id, page: 'tickets', query: '' });
  });

  it('falls back safely on unknown input', () => {
    expect(parseHash('')).toEqual({ workspaceId: null, page: 'dashboard', query: '' });
    expect(parseHash('#/x/y')).toEqual({ workspaceId: null, page: 'dashboard', query: '' });
    expect(parseHash('#/w/abc/unknown')).toEqual({ workspaceId: 'abc', page: 'dashboard', query: '' });
    expect(parseHash('#/x?w=1')).toEqual({ workspaceId: null, page: 'dashboard', query: '' });
  });

  it('derives navigation only from server permissions (default deny)', () => {
    expect(allowedPages([], false).map((p) => p.id)).toEqual([]);
    expect(allowedPages([], true).map((p) => p.id)).toEqual(['components']);
    const cs = ['member.read', 'catalog.read', 'ticket.read', 'report.view'] as const;
    expect(allowedPages(cs, false).map((p) => p.id)).toEqual(['dashboard', 'tickets', 'reports', 'settings']);
    expect(allowedPages(['integration.recovery'], false).map((p) => p.id)).toEqual(['integrations']);
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
});
