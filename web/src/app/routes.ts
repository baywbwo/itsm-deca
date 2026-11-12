import type { DashboardPeriodKey, PermissionCode, TicketFilter } from '../api/types';

export type PageId = 'dashboard' | 'tickets' | 'reports' | 'settings' | 'integrations' | 'users';

export interface PageDef {
  id: PageId;
  label: string;
  /** Visible when the server grants ANY of these here; empty = no domain permission needed. */
  anyOf: readonly PermissionCode[];
  /** Only in UAT mode (design/engineering aid, not a product screen). */
  uatOnly?: boolean;
  /** CP3b: platform-wide page, shown only when the session holds platform_admin (not a workspace permission). */
  platformAdmin?: boolean;
  /** In the "Administrasi" nav section (the caption sits before the first one shown). */
  admin?: boolean;
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
    admin: true,
    anyOf: ['admin.settings', 'admin.members', 'member.read', 'catalog.read'],
    milestone: 'D09',
  },
  {
    id: 'integrations',
    label: 'Integrasi',
    admin: true,
    anyOf: ['integration.manage', 'integration.credentials', 'integration.recovery'],
    milestone: 'D09–D10',
  },
  { id: 'users', label: 'Pengguna', anyOf: [], platformAdmin: true, admin: true, milestone: 'CP3b' },
];

export const pageDef = (id: PageId): PageDef => PAGES.find((p) => p.id === id) ?? PAGES[0]!;

export function allowedPages(permissions: readonly PermissionCode[], uat: boolean, platformAdmin = false): PageDef[] {
  return PAGES.filter((p) => (p.platformAdmin
    ? platformAdmin
    : (!p.uatOnly || uat) && (p.anyOf.length === 0 || p.anyOf.some((c) => permissions.includes(c)))));
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

export type TicketView = 'list' | 'board';
export const TICKET_FILTERS = [
  'q', 'status', 'owner', 'channel', 'category', 'priority',
  'receivedFrom', 'receivedBefore', 'resolvedFrom', 'resolvedBefore',
] as const;
export type TicketFilterKey = (typeof TICKET_FILTERS)[number];
export type TicketSort = 'updated_at' | 'received_at' | 'number';
export type TicketOrder = 'asc' | 'desc';

/**
 * Ticket List/Board state kept in the URL (`#/w/<ws>/tickets?view=board&status=…`). The filter
 * names are the List API's own, so a Dashboard drilldown link opens the same scope, and switching
 * view keeps every filter. Raw values (UUID, `unassigned`/`none`, comma lists, RFC 3339 instants)
 * are validated by the server, which answers 400 for anything outside the contract.
 */
export interface TicketParams {
  view: TicketView;
  filters: Record<TicketFilterKey, string | null>;
  sort: TicketSort;
  order: TicketOrder;
  /** Optional List columns shown (canonical order, never empty). Display only; not sent to the API. */
  columns: TicketColumn[];
}

/** C32 properties: optional List columns in display order (number and title are always shown). */
export const TICKET_COLUMNS = ['status', 'priority', 'owner', 'channel', 'received', 'updated'] as const;
export type TicketColumn = (typeof TICKET_COLUMNS)[number];
export const COLUMN_LABELS: Record<TicketColumn, string> = {
  status: 'Status', priority: 'Prioritas', owner: 'PIC', channel: 'Kanal · kategori',
  received: 'Diterima', updated: 'Diperbarui',
};

/** Allowlisted, deduplicated, canonical order; unknown names are dropped, nothing left = all. */
export function canonicalColumns(raw: readonly string[]): TicketColumn[] {
  const picked = TICKET_COLUMNS.filter((c) => raw.includes(c));
  return picked.length ? picked : [...TICKET_COLUMNS];
}
const allColumns = (c: readonly TicketColumn[]) => c.length === TICKET_COLUMNS.length;

const SORTS: readonly string[] = ['updated_at', 'received_at', 'number'];

export function parseTicketQuery(query: string): TicketParams {
  const q = new URLSearchParams(query);
  const view = q.get('view') === 'board' ? 'board' : 'list';
  const filters = Object.fromEntries(
    TICKET_FILTERS.map((k) => [k, (k === 'q' ? q.get(k)?.trim() : q.get(k)) || null]),
  ) as TicketParams['filters'];
  const s = q.get('sort');
  const sort = (s && SORTS.includes(s) ? s : 'updated_at') as TicketSort;
  const order = q.get('order') === 'asc' ? 'asc' : 'desc';
  const columns = canonicalColumns((q.get('cols') ?? '').split(','));
  return { view, filters, sort, order, columns };
}

/** Canonical API query (no `view`): the same scope always gives the same string and cache key. */
export function ticketApiQuery(p: TicketParams): string {
  const q = new URLSearchParams();
  for (const k of TICKET_FILTERS) {
    const v = p.filters[k];
    if (v) q.set(k, v);
  }
  if (p.sort !== 'updated_at') q.set('sort', p.sort);
  if (p.order !== 'desc') q.set('order', p.order);
  return q.toString();
}

/** URL query: view first (default list omitted), then the API query, then non-default columns. */
export function ticketQuery(p: TicketParams): string {
  const parts = [p.view === 'board' ? 'view=board' : '', ticketApiQuery(p),
    allColumns(p.columns) ? '' : `cols=${canonicalColumns(p.columns).join(',')}`];
  return parts.filter(Boolean).join('&');
}

/** Filters that narrow the scope (search included; sort is not a filter). */
export const activeTicketFilters = (p: TicketParams) => TICKET_FILTERS.filter((k) => p.filters[k]);

export const emptyTicketFilters = (): TicketParams['filters'] =>
  Object.fromEntries(TICKET_FILTERS.map((k) => [k, null])) as TicketParams['filters'];

/** Filters a saved view (C32) may hold: everything except free-text search (may hold PII). */
export const SAVED_VIEW_FILTERS = TICKET_FILTERS.filter((k) => k !== 'q');

/** Saved-view query (string values) for the current scope; search text is never included. */
export function savedViewQuery(p: TicketParams): Record<string, string> {
  const q: Record<string, string> = {};
  for (const k of SAVED_VIEW_FILTERS) {
    const v = p.filters[k];
    if (v) q[k] = v;
  }
  q.sort = p.sort;
  q.order = p.order;
  return q;
}

/** Ticket params a saved view opens: its view, filters, sort and columns; search is cleared. */
export function paramsFromSavedView(v: {
  view: string;
  query: Record<string, string | undefined>;
  columns?: readonly string[];
}): TicketParams {
  const q = new URLSearchParams();
  if (v.view === 'board') q.set('view', 'board');
  for (const k of [...SAVED_VIEW_FILTERS, 'sort', 'order'] as const) {
    const value = v.query[k];
    if (value) q.set(k, value);
  }
  return { ...parseTicketQuery(q.toString()), columns: canonicalColumns(v.columns ?? []) };
}

/** Same view, filters, sort and columns (search ignored): the saved view that matches the screen. */
export function sameSavedScope(a: TicketParams, b: TicketParams): boolean {
  const strip = (p: TicketParams) => ticketQuery({ ...p, filters: { ...p.filters, q: null } });
  return strip(a) === strip(b);
}

/** Same as [sameSavedScope] except the columns: the view only needs its columns updated. */
export const sameFilterScope = (a: TicketParams, b: TicketParams) =>
  sameSavedScope({ ...a, columns: [...TICKET_COLUMNS] }, { ...b, columns: [...TICKET_COLUMNS] });

/**
 * True when going from `from` to `to` takes the user out of the page or workspace they are in.
 * Query-only changes (filters, sort, view) stay put and never trigger the draft guard.
 */
export const leavesScope = (from: Route, to: Route) =>
  from.workspaceId !== to.workspaceId || from.page !== to.page;

/**
 * D08 deep link on top of the ticket query: `ticket=<id>` opens the side peek, `detail=full` the
 * full-page mode of the same ticket. Neither is part of the List API scope.
 */
export interface DetailParams { ticketId: string | null; full: boolean }

export function parseDetailQuery(query: string): DetailParams {
  const q = new URLSearchParams(query);
  const ticketId = q.get('ticket')?.trim() || null;
  return { ticketId, full: !!ticketId && q.get('detail') === 'full' };
}

export function withDetail(query: string, ticketId: string | null, full = false): string {
  const q = new URLSearchParams(query);
  q.delete('ticket');
  q.delete('detail');
  if (ticketId) {
    q.set('ticket', ticketId);
    if (full) q.set('detail', 'full');
  }
  return q.toString();
}

// --- D10 reports -------------------------------------------------------------
export const REPORT_STATUSES = ['new', 'open', 'pending_customer', 'pending_engineering', 'resolved', 'closed'] as const;
export type ReportStatus = (typeof REPORT_STATUSES)[number];

/** Report state in the URL: the same names the API takes, so the URL is the preview/export spec. */
export interface ReportParams {
  periodStart: string;
  periodEnd: string;
  basis: 'received' | 'resolved';
  /** Empty = every status. Canonical order, no duplicates. */
  status: ReportStatus[];
  filters: Record<DashboardFilterKey, string | null>;
}

const DAY = /^\d{4}-\d{2}-\d{2}$/;

/** First day of the current month to today, in the workspace time zone. */
export function defaultReportPeriod(timeZone: string, now = Date.now()): { periodStart: string; periodEnd: string } {
  const today = new Intl.DateTimeFormat('en-CA', { timeZone, year: 'numeric', month: '2-digit', day: '2-digit' }).format(now);
  return { periodStart: `${today.slice(0, 8)}01`, periodEnd: today };
}

/** Missing or malformed dates fall back to the default period; the server still validates the rest. */
export function parseReportQuery(query: string, timeZone: string, now = Date.now()): ReportParams {
  const q = new URLSearchParams(query);
  const def = defaultReportPeriod(timeZone, now);
  const s = q.get('periodStart') ?? '';
  const e = q.get('periodEnd') ?? '';
  const raw = (q.get('status') ?? '').split(',');
  return {
    periodStart: DAY.test(s) ? s : def.periodStart,
    periodEnd: DAY.test(e) ? e : def.periodEnd,
    basis: q.get('basis') === 'resolved' ? 'resolved' : 'received',
    status: REPORT_STATUSES.filter((c) => raw.includes(c)),
    filters: Object.fromEntries(DASHBOARD_FILTERS.map((k) => [k, q.get(k) || null])) as ReportParams['filters'],
  };
}

/** Canonical query (fixed order): the URL, the preview request and the export spec all use it. */
export function reportQuery(p: ReportParams): string {
  const q = new URLSearchParams();
  q.set('periodStart', p.periodStart);
  q.set('periodEnd', p.periodEnd);
  q.set('basis', p.basis);
  if (p.status.length) q.set('status', REPORT_STATUSES.filter((c) => p.status.includes(c)).join(','));
  for (const k of DASHBOARD_FILTERS) {
    const v = p.filters[k];
    if (v) q.set(k, v);
  }
  return q.toString();
}

/** Body of the export request for exactly the previewed spec, as CSV or a real XLSX workbook. */
export function reportExportBody(p: ReportParams, format: 'csv' | 'xlsx' = 'csv') {
  return {
    reportType: 'operational' as const,
    format,
    periodStart: p.periodStart,
    periodEnd: p.periodEnd,
    basis: p.basis,
    ...(p.status.length ? { status: REPORT_STATUSES.filter((c) => p.status.includes(c)) } : {}),
    ...Object.fromEntries(DASHBOARD_FILTERS.flatMap((k) => (p.filters[k] ? [[k, p.filters[k]]] : []))),
  };
}

/** CP-UI1 sidebar entry (owner prototype groups Operasional / Analitik / Administrasi). */
export interface NavItem {
  key: PageId | 'board' | 'new';
  label: string;
  href: string;
  current: boolean;
}
export interface NavGroup { label: string; items: NavItem[] }

/** Query flag of the "Tiket baru" link: Tiket opens the intake form once, then drops it. */
export const NEW_TICKET_FLAG = 'new';

/**
 * Sidebar groups from the pages the server allows here (default deny, same as before). Board is
 * the Tiket page in board view and "Tiket baru" its intake form, so neither adds a route or a
 * permission: both need Tiket, and "Tiket baru" also `ticket.create`. Empty groups are left out.
 */
export function navGroups(pages: readonly PageDef[], route: Route, canCreate: boolean): NavGroup[] {
  const ws = route.workspaceId;
  if (!ws) return [];
  const has = (id: PageId) => pages.some((p) => p.id === id);
  const board = route.page === 'tickets' && parseTicketQuery(route.query).view === 'board';
  const item = (id: PageId): NavItem => ({
    key: id, label: pageDef(id).label, href: toHash({ workspaceId: ws, page: id }),
    current: route.page === id && !(id === 'tickets' && board),
  });
  const ops: NavItem[] = [];
  if (has('dashboard')) ops.push(item('dashboard'));
  if (has('tickets')) {
    ops.push(item('tickets'));
    ops.push({ key: 'board', label: 'Board', href: toHash({ workspaceId: ws, page: 'tickets', query: 'view=board' }), current: board });
    if (canCreate) {
      ops.push({ key: 'new', label: 'Tiket baru', href: toHash({ workspaceId: ws, page: 'tickets', query: `${NEW_TICKET_FLAG}=1` }), current: false });
    }
  }
  const groups: NavGroup[] = [
    { label: 'Operasional', items: ops },
    { label: 'Analitik', items: has('reports') ? [item('reports')] : [] },
    { label: 'Administrasi', items: pages.filter((p) => p.admin).map((p) => item(p.id)) },
  ];
  return groups.filter((g) => g.items.length > 0);
}
