import type { DashboardPeriodKey } from '../api/types';

const NUM = new Intl.NumberFormat('id-ID');
export const num = (n: number) => NUM.format(n);

const DAY = new Intl.DateTimeFormat('id-ID', { day: 'numeric', month: 'short', year: 'numeric', timeZone: 'UTC' });
const DAY_SHORT = new Intl.DateTimeFormat('id-ID', { day: 'numeric', month: 'short', timeZone: 'UTC' });

/** Calendar date `YYYY-MM-DD` (already local to the workspace) → "1 Nov 2026". */
function toUtcDate(d: string): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(d);
  return m ? new Date(Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3]))) : null;
}
export function dayLabel(d: string, short = false): string {
  const date = toUtcDate(d);
  return date ? (short ? DAY_SHORT : DAY).format(date) : d;
}

/** Inclusive local date range → "1 Nov 2026" or "26 Okt – 1 Nov 2026". */
export function rangeLabel(start: string, end: string): string {
  return start === end ? dayLabel(start) : `${dayLabel(start, true)} – ${dayLabel(end)}`;
}

/** Instant in the workspace time zone, e.g. "1 Nov 2026, 14.05". */
export function instantLabel(iso: string, timeZone: string): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  return new Intl.DateTimeFormat('id-ID', { dateStyle: 'medium', timeStyle: 'short', timeZone }).format(t);
}

/** "Asia/Jakarta" → "Jakarta". */
export const tzLabel = (tz: string) => (tz.split('/').pop() ?? tz).replace(/_/g, ' ');

export function ageLabel(seconds: number): string {
  if (seconds < 3600) return `${Math.max(1, Math.floor(seconds / 60))} menit`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)} jam`;
  return `${Math.floor(seconds / 86400)} hari`;
}

export const PERIOD_LABELS: Record<DashboardPeriodKey, string> = {
  today: 'Hari ini',
  last_7_days: '7 hari terakhir',
  this_month: 'Bulan ini',
};
