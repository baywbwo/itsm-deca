import { describe, expect, it } from 'vitest';
import type { Dashboard } from '../api/types';
import { attentionQuery, barPercent, channelSlices, dataQuality, donutGradient, resolvedKpiLabel } from './dashboardView';
import { parseDashboardQuery, parseTicketQuery } from './routes';

const dim = (id: string | null, label: string | null, count: number) =>
  ({ id, code: null, label, count, filter: {} }) as Dashboard['channels'][number];

describe('CP-UI2b dashboard defaults', () => {
  it('opens on this month', () => {
    expect(parseDashboardQuery('').period).toBe('this_month');
    expect(parseDashboardQuery('period=bogus').period).toBe('this_month');
    expect(parseDashboardQuery('period=today').period).toBe('today');
  });

  it('Perlu perhatian reads the unassigned drilldown oldest first through the List', () => {
    const q = attentionQuery({ status: ['new', 'open'], owner: 'unassigned', channel: 'c1' });
    expect(q).toBe('status=new%2Copen&owner=unassigned&channel=c1&sort=received_at&order=asc');
    const p = parseTicketQuery(q);
    expect([p.filters.owner, p.filters.status, p.sort, p.order]).toEqual(['unassigned', 'new,open', 'received_at', 'asc']);
  });
});

describe('CP-UI2 dashboard view', () => {
  it('names the resolved KPI after the selected period', () => {
    expect(resolvedKpiLabel('this_month')).toBe('Selesai bulan ini');
    expect(resolvedKpiLabel('today')).toBe('Selesai hari ini');
    expect(resolvedKpiLabel('last_7_days')).toBe('Selesai 7 hari terakhir');
  });

  it('channel donut drops empty rows and labels the no-channel row', () => {
    const s = channelSlices([dim('a', 'WhatsApp', 3), dim('b', 'Email', 0), dim(null, null, 1)]);
    expect(s.map((x) => [x.label, x.count])).toEqual([['WhatsApp', 3], ['Tanpa kanal', 1]]);
    expect(donutGradient(s)).toBe('conic-gradient(#003571 0.00% 75.00%, #c3ccd6 75.00% 100.00%)');
    expect(donutGradient([])).toContain('var(--line-soft)');
  });

  it('data quality reads only server numbers, each with its basis', () => {
    const d = {
      totals: { unassigned: 3, active: 9, receivedInPeriod: 12 },
      categories: [dim('c', 'QRIS', 10), dim(null, null, 2)],
      period: { key: 'this_month' },
    } as unknown as Dashboard;
    expect(dataQuality(d)).toEqual([
      { key: 'owner', label: 'Tanpa PIC', value: 3, of: 9, basis: 'tiket aktif saat ini' },
      { key: 'category', label: 'Tanpa kategori', value: 2, of: 12, basis: 'tiket diterima bulan ini' },
    ]);
    expect(dataQuality({ ...d, categories: [] }).at(1)?.value).toBe(0);
  });

  it('bars never divide by zero', () => {
    expect(barPercent(0, 0)).toBe(0);
    expect(barPercent(1, 3)).toBe(33);
  });
});
