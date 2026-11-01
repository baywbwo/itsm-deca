import { describe, expect, it } from 'vitest';
import { canonicalColumns, paramsFromSavedView, parseTicketQuery, sameFilterScope, sameSavedScope, savedViewQuery, ticketApiQuery, ticketQuery, TICKET_COLUMNS } from './routes';

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

  it('columns: allowlisted, canonical, in the URL but never in the API query, part of the saved scope', () => {
    expect(canonicalColumns(['updated', 'x', 'status', 'status'])).toEqual(['status', 'updated']);
    expect(canonicalColumns([])).toEqual([...TICKET_COLUMNS]);
    const all = parseTicketQuery('status=open');
    expect(all.columns).toEqual([...TICKET_COLUMNS]);
    expect(ticketQuery(all)).not.toContain('cols=');
    const some = parseTicketQuery('status=open&cols=updated,owner,bogus');
    expect(some.columns).toEqual(['owner', 'updated']);
    expect(decodeURIComponent(ticketQuery(some))).toContain('cols=owner,updated');
    expect(ticketApiQuery(some)).not.toContain('cols');
    const saved = paramsFromSavedView({ view: 'list', query: savedViewQuery(all), columns: ['status'] });
    expect(saved.columns).toEqual(['status']);
    expect(sameFilterScope(saved, all)).toBe(true);
    expect(sameSavedScope(saved, all)).toBe(false);
    expect(sameSavedScope(saved, { ...all, columns: ['status'] })).toBe(true);
  });
});
