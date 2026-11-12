import type { Dashboard, DashboardPeriodKey, TicketFilter } from '../api/types';
import { PERIOD_LABELS } from './format';
import { filterToQuery } from './routes';

/** CP-UI2b "Perlu perhatian": at most this many tickets on the dashboard (prototype shows 5-6). */
export const ATTENTION_LIMIT = 6;

/**
 * CP-UI2b: "Perlu perhatian" = the prototype's "belum ada PIC, urut umur". It is the server's own
 * "Belum ditugaskan" drilldown (active, no PIC, dashboard filters) read through the ticket List,
 * oldest received first. The same query (without the limit) is the "Lihat semua" link.
 */
export function attentionQuery(unassigned: TicketFilter): string {
  const q = new URLSearchParams(filterToQuery(unassigned));
  q.set('sort', 'received_at');
  q.set('order', 'asc');
  return q.toString();
}

/** CP-UI2: "Selesai bulan ini" in the owner prototype; the label follows the selected period. */
export function resolvedKpiLabel(period: DashboardPeriodKey): string {
  return period === 'this_month' ? 'Selesai bulan ini' : `Selesai ${PERIOD_LABELS[period].toLowerCase()}`;
}

/** Fixed channel palette (prototype donut order); colour is never the only cue, the legend has text. */
export const CHANNEL_COLORS = ['#003571', '#4c8dd6', '#58ea60', '#e9a23b', '#8a9aac', '#7c5cd6'] as const;

export interface DonutSlice { key: string; label: string; count: number; color: string }

/**
 * Donut slices for "Kanal masuk" from the server rows (tickets received in the period). Rows
 * with no tickets are dropped; colours past the palette repeat its last entry.
 */
export function channelSlices(rows: Dashboard['channels'], none = 'Tanpa kanal'): DonutSlice[] {
  return rows.filter((r) => r.count > 0).map((r, i) => ({
    key: r.id ?? 'none',
    label: r.label ?? none,
    count: r.count,
    color: r.id ? CHANNEL_COLORS[Math.min(i, CHANNEL_COLORS.length - 1)] ?? '#8a9aac' : '#c3ccd6',
  }));
}

/** CSS conic-gradient stops for the slices; an empty list gives the neutral track only. */
export function donutGradient(slices: readonly DonutSlice[]): string {
  const total = slices.reduce((n, s) => n + s.count, 0);
  if (total === 0) return 'conic-gradient(var(--line-soft) 0 100%)';
  let at = 0;
  const stops = slices.map((s) => {
    const from = at;
    at += (s.count / total) * 100;
    return `${s.color} ${from.toFixed(2)}% ${at.toFixed(2)}%`;
  });
  return `conic-gradient(${stops.join(', ')})`;
}

export interface QualityRow { key: string; label: string; value: number; of: number; basis: string }

/**
 * "Kualitas data" from numbers the dashboard already returns, each with its own basis:
 * no PIC = active tickets now; no category = tickets received in the period (categories row
 * with no id). Nothing is estimated.
 */
export function dataQuality(d: Pick<Dashboard, 'totals' | 'categories' | 'period'>): QualityRow[] {
  const noCategory = d.categories.find((c) => c.id === null)?.count ?? 0;
  return [
    { key: 'owner', label: 'Tanpa PIC', value: d.totals.unassigned, of: d.totals.active, basis: 'tiket aktif saat ini' },
    {
      key: 'category', label: 'Tanpa kategori', value: noCategory, of: d.totals.receivedInPeriod,
      basis: `tiket diterima ${PERIOD_LABELS[d.period.key].toLowerCase()}`,
    },
  ];
}

/** Bar width in percent of the largest value (0 when everything is 0). */
export function barPercent(value: number, max: number): number {
  return max > 0 ? Math.round((value / max) * 100) : 0;
}
