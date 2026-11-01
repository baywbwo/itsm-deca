import { describe, expect, it } from 'vitest';
import { localDate, nextDay, zonedMidnight } from './format';
import {
  activeTicketFilters,
  emptyTicketFilters,
  filterToQuery,
  parseHash,
  parseTicketQuery,
  ticketApiQuery,
  ticketQuery,
  drilldownHash,
} from './routes';

const ws = '7f1c2a8e-0000-4000-8000-000000000001';
const owner = '7f1c2a8e-0000-4000-8000-0000000000aa';

describe('ticket URL state', () => {
  it('a dashboard drilldown opens the same API scope in the List', () => {
    const f = {
      status: ['new', 'open'] as ('new' | 'open')[],
      owner,
      resolvedFrom: '2026-10-31T17:00:00Z',
      resolvedBefore: '2026-11-01T17:00:00Z',
    };
    const route = parseHash(drilldownHash(ws, f));
    expect(route.page).toBe('tickets');
    const p = parseTicketQuery(route.query);
    expect(p.view).toBe('list');
    expect(ticketApiQuery(p)).toBe(filterToQuery(f));
  });

  it('switching view keeps every filter and the sort', () => {
    const p = parseTicketQuery('status=pending_customer&owner=unassigned&q=%20dana%20&sort=number&order=asc');
    expect(p.filters.q).toBe('dana');
    const board = parseTicketQuery(ticketQuery({ ...p, view: 'board' }));
    expect(board.view).toBe('board');
    expect(board.filters).toEqual(p.filters);
    expect([board.sort, board.order]).toEqual(['number', 'asc']);
    expect(ticketQuery({ ...board, view: 'list' })).toBe(ticketApiQuery(p));
  });

  it('canonical API query omits defaults and the view; unknown sort falls back', () => {
    const p = parseTicketQuery('view=board&sort=evil&order=sideways&cursor=x&channel=none');
    expect([p.sort, p.order]).toEqual(['updated_at', 'desc']);
    expect(ticketApiQuery(p)).toBe('channel=none');
    expect(ticketQuery(p)).toBe('view=board&channel=none');
    expect(activeTicketFilters(p)).toEqual(['channel']);
    expect(activeTicketFilters({ ...p, filters: emptyTicketFilters() })).toEqual([]);
  });
});

describe('workspace-local dates', () => {
  it('maps a Jakarta calendar day to its half-open UTC range', () => {
    expect(zonedMidnight('2026-11-01', 'Asia/Jakarta')).toBe('2026-10-31T17:00:00Z');
    expect(zonedMidnight(nextDay('2026-11-01'), 'Asia/Jakarta')).toBe('2026-11-01T17:00:00Z');
    expect(localDate('2026-10-31T17:00:00Z', 'Asia/Jakarta')).toBe('2026-11-01');
    expect(localDate('2026-11-01T17:00:00Z', 'Asia/Jakarta', true)).toBe('2026-11-01');
  });

  it('handles DST zones and month ends', () => {
    expect(zonedMidnight('2026-03-08', 'America/New_York')).toBe('2026-03-08T05:00:00Z');
    expect(zonedMidnight('2026-03-09', 'America/New_York')).toBe('2026-03-09T04:00:00Z');
    expect(nextDay('2026-12-31')).toBe('2027-01-01');
    expect(zonedMidnight('31-12-2026', 'Asia/Jakarta')).toBeNull();
    expect(localDate('nope', 'Asia/Jakarta')).toBeNull();
  });
});
