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

/** Offset (minutes east of UTC) of `timeZone` at instant `t`. */
function zoneOffset(t: number, timeZone: string): number {
  const parts = new Intl.DateTimeFormat('en-US', {
    timeZone, hourCycle: 'h23', year: 'numeric', month: '2-digit', day: '2-digit',
    hour: '2-digit', minute: '2-digit', second: '2-digit',
  }).formatToParts(t);
  const n = (type: string) => Number(parts.find((p) => p.type === type)?.value);
  const asUtc = Date.UTC(n('year'), n('month') - 1, n('day'), n('hour'), n('minute'), n('second'));
  return Math.round((asUtc - Math.floor(t / 1000) * 1000) / 60000);
}

/** Local calendar date `YYYY-MM-DD` → the UTC instant of its midnight in `timeZone`. */
export function zonedMidnight(date: string, timeZone: string): string | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  if (!m) return null;
  const guess = Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  let t = guess - zoneOffset(guess, timeZone) * 60000;
  t = guess - zoneOffset(t, timeZone) * 60000;
  return new Date(t).toISOString().replace('.000Z', 'Z');
}

/** Instant → local calendar date in `timeZone` (`YYYY-MM-DD`); `endExclusive` gives the last included day. */
export function localDate(iso: string, timeZone: string, endExclusive = false): string | null {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return null;
  return new Intl.DateTimeFormat('en-CA', { timeZone, year: 'numeric', month: '2-digit', day: '2-digit' })
    .format(endExclusive ? t - 1 : t);
}

/** Next calendar day of `YYYY-MM-DD`. */
export function nextDay(date: string): string {
  const [y, m, d] = date.split('-').map(Number);
  return new Date(Date.UTC(y!, m! - 1, d! + 1)).toISOString().slice(0, 10);
}
