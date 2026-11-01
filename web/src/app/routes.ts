import type { DashboardPeriodKey, PermissionCode, TicketFilter } from '../api/types';

export type PageId = 'dashboard' | 'tickets' | 'reports' | 'settings' | 'integrations' | 'components';

export interface PageDef {
  id: PageId;
  label: string;
  /** Visible when the server grants ANY of these here; empty = no domain permission needed. */
  anyOf: readonly PermissionCode[];
  /** Only in UAT mode (design/engineering aid, not a product screen). */
  uatOnly?: boolean;
  /** Delivery step that builds the real screen. */
  milestone: string;
}

export const PAGES: readonly PageDef[] = [
  { id: 'dashboard', label: 'Dashboard', anyOf: ['ticket.read'], milestone: 'D05' },
  { id: 'tickets', label: 'Tiket', anyOf: ['ticket.read'], milestone: 'D06–D08' },
  { id: 'reports', label: 'Laporan', anyOf: ['report.view'], milestone: 'D10' },
  {
    id: 'settings',
    label: 'Pengaturan',
    anyOf: ['admin.settings', 'admin.members', 'member.read', 'catalog.read'],
    milestone: 'D09',
  },
  {
    id: 'integrations',
    label: 'Integrasi',
    anyOf: ['integration.manage', 'integration.credentials', 'integration.recovery'],
    milestone: 'D09–D10',
  },
  { id: 'components', label: 'Komponen', anyOf: [], uatOnly: true, milestone: 'D04' },
];

export const pageDef = (id: PageId): PageDef => PAGES.find((p) => p.id === id) ?? PAGES[0]!;

export function allowedPages(permissions: readonly PermissionCode[], uat: boolean): PageDef[] {
  return PAGES.filter(
    (p) => (!p.uatOnly || uat) && (p.anyOf.length === 0 || p.anyOf.some((c) => permissions.includes(c))),
  );
}

export interface Route {
  workspaceId: string | null;
  page: PageId;
  /** Raw query after `?` (drilldown filters); '' when absent. */
  query: string;
}

const PAGE_IDS = new Set<string>(PAGES.map((p) => p.id));

/** `#/w/<workspaceId>/<page>[?query]`; anything else falls back to no workspace + dashboard. */
export function parseHash(hash: string): Route {
  const raw = hash.replace(/^#\/?/, '');
  const q = raw.indexOf('?');
  const path = q < 0 ? raw : raw.slice(0, q);
  const query = q < 0 ? '' : raw.slice(q + 1);
  const parts = path.split('/').filter(Boolean);
  if (parts[0] !== 'w' || !parts[1]) return { workspaceId: null, page: 'dashboard', query: '' };
  const page = parts[2] && PAGE_IDS.has(parts[2]) ? (parts[2] as PageId) : 'dashboard';
  return { workspaceId: decodeURIComponent(parts[1]), page, query };
}

export const toHash = (r: { workspaceId: string; page: PageId; query?: string }) =>
  `#/w/${encodeURIComponent(r.workspaceId)}/${r.page}${r.query ? `?${r.query}` : ''}`;

/** Server-built drilldown filter → ticket-list query (lists joined by commas). */
export function filterToQuery(f: TicketFilter): string {
  const q = new URLSearchParams();
  if (f.status?.length) q.set('status', f.status.join(','));
  if (f.owner) q.set('owner', f.owner);
  if (f.channel) q.set('channel', f.channel);
  if (f.category) q.set('category', f.category);
  if (f.priority?.length) q.set('priority', f.priority.join(','));
  if (f.receivedFrom) q.set('receivedFrom', f.receivedFrom);
  if (f.receivedBefore) q.set('receivedBefore', f.receivedBefore);
  if (f.resolvedFrom) q.set('resolvedFrom', f.resolvedFrom);
  if (f.resolvedBefore) q.set('resolvedBefore', f.resolvedBefore);
  return q.toString();
}

export const drilldownHash = (workspaceId: string, f: TicketFilter) =>
  toHash({ workspaceId, page: 'tickets', query: filterToQuery(f) });

export const DASHBOARD_FILTERS = ['owner', 'channel', 'category', 'priority'] as const;
export type DashboardFilterKey = (typeof DASHBOARD_FILTERS)[number];

/** Dashboard state kept in the URL so reload, back and shared links show the same numbers. */
export interface DashboardParams {
  period: DashboardPeriodKey;
  /** Raw values (UUID or `unassigned`/`none`); the server validates them against the workspace. */
  filters: Record<DashboardFilterKey, string | null>;
}

const PERIOD_KEYS: readonly string[] = ['today', 'last_7_days', 'this_month'];

/** Unknown period → today; empty filter values are dropped (no filter). */
export function parseDashboardQuery(query: string): DashboardParams {
  const q = new URLSearchParams(query);
  const p = q.get('period');
  const period = (p && PERIOD_KEYS.includes(p) ? p : 'today') as DashboardPeriodKey;
  const filters = Object.fromEntries(DASHBOARD_FILTERS.map((k) => [k, q.get(k) || null])) as DashboardParams['filters'];
  return { period, filters };
}

/** Canonical query in a fixed order; the same params always give the same string. */
export function dashboardQuery(p: DashboardParams): string {
  const q = new URLSearchParams();
  q.set('period', p.period);
  for (const k of DASHBOARD_FILTERS) {
    const v = p.filters[k];
    if (v) q.set(k, v);
  }
  return q.toString();
}

export const hasDashboardFilter = (p: DashboardParams) => DASHBOARD_FILTERS.some((k) => p.filters[k]);
