import { describe, expect, it } from 'vitest';
import { paramsFromSavedView, parseTicketQuery, sameSavedScope, savedViewQuery } from './routes';

describe('C32 saved view routes', () => {
  const p = parseTicketQuery('view=board&q=rahasia&status=open&priority=p1,p2&owner=none&sort=received_at&order=asc');

  it('never stores search text; keeps filters and sort', () => {
    const q = savedViewQuery(p);
    expect('q' in q).toBe(false);
    expect(q).toMatchObject({ status: 'open', priority: 'p1,p2', owner: 'none', sort: 'received_at', order: 'asc' });
  });

  it('applying a view restores view/filters/sort and clears search', () => {
    const back = paramsFromSavedView({ view: 'board', query: savedViewQuery(p) });
    expect(back.view).toBe('board');
    expect(back.filters.q ?? null).toBeNull();
    expect(back.filters.priority).toBe('p1,p2');
    expect([back.sort, back.order]).toEqual(['received_at', 'asc']);
    expect(sameSavedScope(back, p)).toBe(true);
  });

  it('ignores unknown or forbidden keys from storage and detects a different scope', () => {
    const back = paramsFromSavedView({ view: 'list', query: { q: 'x', cursor: 'c', status: 'closed' } });
    expect(back.filters.q ?? null).toBeNull();
    expect(back.view).toBe('list');
    expect(sameSavedScope(back, p)).toBe(false);
    expect(sameSavedScope(back, parseTicketQuery('status=closed&q=apa'))).toBe(true);
  });
});
