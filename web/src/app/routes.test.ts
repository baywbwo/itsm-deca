import { describe, expect, it } from 'vitest';
import { allowedPages, parseHash, toHash } from './routes';

describe('routes', () => {
  it('round-trips workspace and page through the hash', () => {
    const id = '7f1c2a8e-0000-4000-8000-000000000001';
    expect(parseHash(toHash({ workspaceId: id, page: 'tickets' }))).toEqual({ workspaceId: id, page: 'tickets' });
  });

  it('falls back safely on unknown input', () => {
    expect(parseHash('')).toEqual({ workspaceId: null, page: 'dashboard' });
    expect(parseHash('#/x/y')).toEqual({ workspaceId: null, page: 'dashboard' });
    expect(parseHash('#/w/abc/unknown')).toEqual({ workspaceId: 'abc', page: 'dashboard' });
  });

  it('derives navigation only from server permissions (default deny)', () => {
    expect(allowedPages([], false).map((p) => p.id)).toEqual([]);
    expect(allowedPages([], true).map((p) => p.id)).toEqual(['components']);
    const cs = ['member.read', 'catalog.read', 'ticket.read', 'report.view'] as const;
    expect(allowedPages(cs, false).map((p) => p.id)).toEqual(['dashboard', 'tickets', 'reports', 'settings']);
    expect(allowedPages(['integration.recovery'], false).map((p) => p.id)).toEqual(['integrations']);
  });
});
