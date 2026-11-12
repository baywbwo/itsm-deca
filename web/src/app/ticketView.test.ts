import { describe, expect, it } from 'vitest';
import type { CatalogItem } from '../api/types';
import { emptyTicketFilters } from './routes';
import { activePreset, allStatusCount, applyPreset, pageRange, presetsFor, priorityDot, shortInstant, urgentPriorityId } from './ticketView';

const me = '7f1c2a8e-0000-4000-8000-0000000000aa';
const urgent = '7f1c2a8e-0000-4000-8000-0000000000ff';
const pri = (id: string, code: string): CatalogItem => ({ id, code, label: code, active: true, sortOrder: 1 });

describe('CP-UI2 preset views', () => {
  it('find the urgent priority by its frozen code, not by label', () => {
    expect(urgentPriorityId([pri('a', 'high'), pri(urgent, 'urgent')])).toBe(urgent);
    expect(urgentPriorityId([pri('a', 'high')])).toBeNull();
    expect(presetsFor(null)).toEqual(['all', 'mine', 'unassigned']);
  });

  it('only set PIC and priority; search, status and other filters stay', () => {
    const f = { ...emptyTicketFilters(), q: 'qris', status: 'open', channel: 'c1', priority: 'p1' };
    const mine = applyPreset(f, 'mine', me, urgent);
    expect(mine).toEqual({ ...f, owner: me, priority: null });
    expect(applyPreset(f, 'unassigned', me, urgent)).toMatchObject({ owner: 'unassigned', priority: null, q: 'qris' });
    expect(applyPreset(f, 'urgent', me, urgent)).toMatchObject({ owner: null, priority: urgent, status: 'open' });
    expect(applyPreset(mine, 'all', me, urgent)).toMatchObject({ owner: null, priority: null, channel: 'c1' });
  });

  it('a preset is shown as active only on an exact PIC/priority match', () => {
    const f = emptyTicketFilters();
    expect(activePreset(f, me, urgent)).toBe('all');
    expect(activePreset({ ...f, owner: me }, me, urgent)).toBe('mine');
    expect(activePreset({ ...f, owner: 'unassigned' }, me, urgent)).toBe('unassigned');
    expect(activePreset({ ...f, priority: urgent }, me, urgent)).toBe('urgent');
    expect(activePreset({ ...f, owner: me, priority: urgent }, me, urgent)).toBeNull();
    expect(activePreset({ ...f, owner: 'someone-else' }, me, urgent)).toBeNull();
  });
});

describe('CP-UI2 list helpers', () => {
  it('"Semua" sums the per-status counts of the server', () => {
    const s = (code: 'new' | 'open', count: number) => ({ status: { code, label: code, terminal: false, archived: false }, count });
    expect(allStatusCount([s('new', 2), s('open', 3)] as never)).toBe(5);
  });

  it('priority dots follow the frozen codes only', () => {
    expect(priorityDot('urgent')).toBe('p-urgent');
    expect(priorityDot('custom')).toBe('p-none');
    expect(priorityDot(undefined)).toBe('p-none');
  });

  it('received time is short and in the workspace time zone', () => {
    expect(shortInstant('2026-11-11T01:12:00Z', 'Asia/Jakarta')).toBe('11-11 08.12');
    expect(shortInstant('2026-10-31T17:30:00Z', 'Asia/Jakarta')).toBe('11-01 00.30');
    expect(shortInstant('nope', 'Asia/Jakarta')).toBe('nope');
  });

  it('page range counts rows of earlier pages', () => {
    expect(pageRange([25, 25, 7], 0)).toEqual({ from: 1, to: 25 });
    expect(pageRange([25, 25, 7], 2)).toEqual({ from: 51, to: 57 });
    expect(pageRange([], 0)).toEqual({ from: 0, to: 0 });
  });
});
