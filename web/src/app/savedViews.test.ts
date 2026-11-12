import { describe, expect, it } from 'vitest';
import { canonicalColumns, crumbLabel, DEFAULT_TICKET_COLUMNS, paramsFromSavedView, ticketApiQuery as apiQ, parseTicketQuery, sameFilterScope, sameSavedScope, savedViewQuery, ticketApiQuery, ticketQuery, TICKET_COLUMNS } from './routes';

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
    const all = parseTicketQuery('status=open&cols=status,priority,owner,channel,received,updated');
    expect(all.columns).toEqual([...TICKET_COLUMNS]);
    expect(decodeURIComponent(ticketQuery(all))).toContain('cols=status,priority,owner,channel,received,updated');
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

describe('CP-UI2b list display defaults', () => {
  it('default columns are the prototype set (no Diperbarui) and stay out of the URL', () => {
    const d = parseTicketQuery('status=open');
    expect(d.columns).toEqual([...DEFAULT_TICKET_COLUMNS]);
    expect(d.columns).not.toContain('updated');
    expect(ticketQuery(d)).toBe('status=open');
    expect(parseTicketQuery('cols=bogus').columns).toEqual([...DEFAULT_TICKET_COLUMNS]);
    // A saved view with no stored columns opens the default set, same as a URL without cols.
    expect(paramsFromSavedView({ view: 'list', query: {} }).columns).toEqual([...DEFAULT_TICKET_COLUMNS]);
  });

  it('row status button is a display flag: URL only, never API or saved scope', () => {
    const off = parseTicketQuery('status=open');
    expect(off.rowActions).toBe(false);
    const on = parseTicketQuery('status=open&acts=1');
    expect(on.rowActions).toBe(true);
    expect(ticketQuery(on)).toBe('status=open&acts=1');
    expect(apiQ(on)).toBe('status=open');
    expect(sameSavedScope(on, off)).toBe(true);
    expect(sameFilterScope(on, off)).toBe(true);
    expect('acts' in savedViewQuery(on)).toBe(false);
  });

  it('breadcrumb names the full detail and the board', () => {
    const r = (page: 'tickets' | 'dashboard', query: string) => ({ workspaceId: 'w', page, query });
    expect(crumbLabel(r('tickets', 'ticket=t1&detail=full'))).toBe('Detail tiket');
    expect(crumbLabel(r('tickets', 'ticket=t1'))).toBe('Tiket');
    expect(crumbLabel(r('tickets', 'view=board'))).toBe('Board');
    expect(crumbLabel(r('dashboard', ''))).toBe('Dashboard');
  });
});
