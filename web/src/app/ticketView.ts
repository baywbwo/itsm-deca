import type { CatalogItem, TicketList } from '../api/types';
import type { TicketParams } from './routes';

/**
 * CP-UI2 preset views of the owner prototype ("Semua tiket", "Tiket saya", …). Each one only
 * sets the existing PIC and priority filters, so it is a URL like any other filter and the API
 * does the work; status, search, channel, category and dates are kept.
 */
export type TicketPreset = 'all' | 'mine' | 'unassigned' | 'urgent';
export const PRESET_LABELS: Record<TicketPreset, string> = {
  all: 'Semua tiket', mine: 'Tiket saya', unassigned: 'Belum ditugaskan', urgent: 'Prioritas urgent',
};

/** Priority id of the `urgent` catalog code (codes are the frozen low/normal/high/urgent). */
export const urgentPriorityId = (priorities: readonly CatalogItem[]): string | null =>
  priorities.find((p) => p.code === 'urgent')?.id ?? null;

/** Presets offered here: "urgent" only when the catalog has that code. */
export function presetsFor(urgentId: string | null): TicketPreset[] {
  return urgentId ? ['all', 'mine', 'unassigned', 'urgent'] : ['all', 'mine', 'unassigned'];
}

export function applyPreset(f: TicketParams['filters'], preset: TicketPreset, userId: string, urgentId: string | null): TicketParams['filters'] {
  const owner = preset === 'mine' ? userId : preset === 'unassigned' ? 'unassigned' : null;
  const priority = preset === 'urgent' ? urgentId : null;
  return { ...f, owner, priority };
}

/** The preset whose PIC/priority pair matches the filters exactly, else null (custom filter). */
export function activePreset(f: TicketParams['filters'], userId: string, urgentId: string | null): TicketPreset | null {
  const { owner, priority } = f;
  if (!owner && !priority) return 'all';
  if (owner === userId && !priority) return 'mine';
  if (owner === 'unassigned' && !priority) return 'unassigned';
  if (!owner && urgentId && priority === urgentId) return 'urgent';
  return null;
}

/** "Semua N": sum of the server's per-status counts (whole scope, every status). */
export const allStatusCount = (counts: TicketList['countsByStatus']): number =>
  counts.reduce((n, c) => n + c.count, 0);

/** Priority dot class from the frozen priority codes; unknown codes get the neutral dot. */
export function priorityDot(code: string | undefined): string {
  return code && ['low', 'normal', 'high', 'urgent'].includes(code) ? `p-${code}` : 'p-none';
}

/** Compact "MM-DD HH.mm" in the workspace time zone (prototype "Diterima" column). */
export function shortInstant(iso: string, timeZone: string): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const parts = Object.fromEntries(new Intl.DateTimeFormat('en-GB', {
    month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hourCycle: 'h23', timeZone,
  }).formatToParts(t).map((p) => [p.type, p.value]));
  return `${parts.month}-${parts.day} ${parts.hour}.${parts.minute}`;
}

/** Client-side pages over the cursor list: "Menampilkan 26–50 dari 120 tiket". */
export function pageRange(pageSizes: readonly number[], index: number): { from: number; to: number } {
  const before = pageSizes.slice(0, index).reduce((n, s) => n + s, 0);
  const size = pageSizes[index] ?? 0;
  return size === 0 ? { from: 0, to: 0 } : { from: before + 1, to: before + size };
}
