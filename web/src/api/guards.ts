// Runtime checks at the network boundary. The compile-time types are generated; these
// only make sure an unexpected payload fails loudly instead of rendering garbage.
import type {
  ApiErrorBody,
  Catalogs,
  Dashboard,
  Member,
  Meta,
  PermissionCode,
  Session,
  SessionIssued,
  TicketBoard,
  TicketList,
  TicketListItem,
  TicketMutationResult,
  Workspace,
  WorkspacePermissions,
} from './types';

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === 'object' && v !== null && !Array.isArray(v);
const isStr = (v: unknown): v is string => typeof v === 'string';
const isStrOrNull = (v: unknown): v is string | null => v === null || isStr(v);
const isRole = (v: unknown) => v === null || v === 'cs' || v === 'lead_cs';

const PERMISSIONS: readonly PermissionCode[] = [
  'member.read', 'catalog.read', 'ticket.read', 'ticket.create', 'ticket.update',
  'ticket.transition', 'ticket.resolve', 'ticket.assign_self', 'ticket.assign_any',
  'related_work.manage', 'attachment.read', 'attachment.upload', 'report.view',
  'admin.members', 'admin.settings', 'integration.manage', 'integration.credentials',
  'integration.recovery',
];
export const isPermissionCode = (v: unknown): v is PermissionCode =>
  isStr(v) && (PERMISSIONS as readonly string[]).includes(v);

export function isMeta(v: unknown): v is Meta {
  if (!isObj(v)) return false;
  if (v.environment !== 'uat' && v.environment !== 'production') return false;
  if (v.accessMode !== 'uat_superadmin' && v.accessMode !== 'sso') return false;
  if (!isStrOrNull(v.banner)) return false;
  const uat = v.uat;
  if (uat === undefined || uat === null) return true;
  return (
    isObj(uat) && isStr(uat.banner) && uat.persistent === true &&
    uat.dataScope === 'dummy' && uat.superadminProvesRbac === false
  );
}

export function isSession(v: unknown): v is Session {
  return (
    isObj(v) && isObj(v.user) && isStr(v.user.id) && isStr(v.user.displayName) &&
    isStrOrNull(v.banner) && typeof v.uatSuperadmin === 'boolean'
  );
}

export const isSessionIssued = (v: unknown): v is SessionIssued =>
  isSession(v) && isStr((v as Obj).csrfToken) && ((v as Obj).csrfToken as string).length > 0;

export function isWorkspace(v: unknown): v is Workspace {
  return (
    isObj(v) && isStr(v.id) && isStr(v.code) && isStr(v.name) && isStr(v.timezone) &&
    isStr(v.companyId) && typeof v.version === 'number' && isRole(v.myRole)
  );
}

export const isWorkspaceList = (v: unknown): v is { items: Workspace[] } =>
  isObj(v) && Array.isArray(v.items) && v.items.every(isWorkspace);

export function isWorkspacePermissions(v: unknown): v is WorkspacePermissions {
  return (
    isObj(v) && isStr(v.workspaceId) && isRole(v.role) && typeof v.uatSuperadmin === 'boolean' &&
    Array.isArray(v.permissions) && v.permissions.every(isPermissionCode) &&
    Array.isArray(v.grants) && v.grants.every(isPermissionCode)
  );
}

export const isErrorBody = (v: unknown): v is ApiErrorBody =>
  isObj(v) && isObj(v.error) && isStr(v.error.code) && isStr(v.error.message);

const isNum = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);
const isArr = (v: unknown, item: (x: unknown) => boolean) => Array.isArray(v) && v.every(item);
const isStatus = (v: unknown) => isObj(v) && isStr(v.code) && isStr(v.label);
const isFilter = (v: unknown) => isObj(v);
const PERIODS = ['today', 'last_7_days', 'this_month'];
const TOTALS = [
  'tickets', 'active', 'unassigned', 'pendingCustomer', 'receivedInPeriod', 'resolvedInPeriod',
  'resolvedTicketsInPeriod',
] as const;

const FILTER_KEYS = ['owner', 'channel', 'category', 'priority'] as const;
const BASES = ['current_status', 'received_at', 'resolution_event', 'resolution_event_distinct_ticket'];
const BASIS_KEYS = [
  'active', 'unassigned', 'pendingCustomer', 'receivedInPeriod', 'resolvedInPeriod', 'resolvedTicketsInPeriod',
  'statusDistribution', 'picLoad', 'age', 'channels', 'categories', 'trendReceived', 'trendResolved', 'attention',
] as const;
const DRILLS = ['active', 'pendingCustomer', 'receivedInPeriod', 'resolvedInPeriod'] as const;
const isOption = (o: unknown) => isObj(o) && isStr(o.id) && isStrOrNull(o.code) && isStr(o.label) && typeof o.active === 'boolean';
const isDimension = (c: unknown) => isObj(c) && isStrOrNull(c.id) && isStrOrNull(c.label) && isNum(c.count) && isFilter(c.filter);

/** Shape check of the dashboard aggregate; numbers must be numbers, lists must be lists. */
export function isDashboard(v: unknown): v is Dashboard {
  if (!isObj(v) || !isStr(v.workspaceId) || !isStr(v.workspaceCode) || !isStr(v.asOf)) return false;
  const p = v.period;
  if (!isObj(p) || !PERIODS.includes(p.key as string) || !isStr(p.timezone) || !isStr(p.startDate) ||
    !isStr(p.endDate) || !isStr(p.startAt) || !isStr(p.endAt)) return false;
  const t = v.totals;
  if (!isObj(t) || !TOTALS.every((k) => isNum(t[k]))) return false;
  const a = v.attention;
  const f = v.filters;
  const o = v.filterOptions;
  const b = v.metricBasis;
  const dr = v.drilldown;
  return (
    isObj(f) && FILTER_KEYS.every((k) => isStrOrNull(f[k])) &&
    isObj(o) && (['owners', 'channels', 'categories', 'priorities'] as const).every((k) => isArr(o[k], isOption)) &&
    isObj(b) && BASIS_KEYS.every((k) => BASES.includes(b[k] as string)) &&
    isObj(dr) && DRILLS.every((k) => isFilter(dr[k])) &&
    (dr.unassigned === null || isFilter(dr.unassigned)) && (dr.attention === null || isFilter(dr.attention)) &&
    typeof v.slaConfigured === 'boolean' &&
    isArr(v.statusDistribution, (s) => isObj(s) && isStatus(s.status) && isNum(s.count) && isFilter(s.filter)) &&
    isArr(v.picLoad, (r) => isObj(r) && isStrOrNull(r.userId) && isStrOrNull(r.displayName) && isNum(r.activeTickets)) &&
    isArr(v.channels, isDimension) && isArr(v.categories, isDimension) &&
    isArr(v.age, (b) => isObj(b) && isStr(b.label) && isNum(b.count)) &&
    isArr(v.trend, (d) => isObj(d) && isStr(d.date) && isNum(d.received) && isNum(d.resolved)) &&
    isObj(a) && a.rule === 'operational_rule' && isNum(a.total) &&
    isArr(a.items, (i) => isObj(i) && isStr(i.id) && isStr(i.displayNumber) && isStr(i.subject) && isStatus(i.status) && isNum(i.ageSeconds))
  );
}

const STATUS_CODES = ['new', 'open', 'pending_customer', 'pending_engineering', 'resolved', 'closed'];
const isTicketStatus = (v: unknown) =>
  isObj(v) && STATUS_CODES.includes(v.code as string) && isStr(v.label) &&
  typeof v.selectable === 'boolean' && typeof v.archived === 'boolean';
const isRefOrNull = (v: unknown) => v === null || (isObj(v) && isStr(v.id) && isStr(v.label));
const isAction = (a: unknown) =>
  isObj(a) && isTicketStatus(a.to) && ['move', 'resolve', 'reopen'].includes(a.kind as string) &&
  ['none', 'summary', 'reason'].includes(a.requires as string) &&
  (a.blockedBy === null || a.blockedBy === 'blocking_related_work');

/** One ticket as List, Board and mutation results return it (no identity/contact data). */
export function isTicketListItem(v: unknown): v is TicketListItem {
  return (
    isObj(v) && isStr(v.id) && isNum(v.number) && isStr(v.displayNumber) && isStr(v.subject) &&
    isTicketStatus(v.status) && isNum(v.cycleNo) && isNum(v.version) &&
    isRefOrNull(v.priority) && isRefOrNull(v.owner) && isRefOrNull(v.channel) && isRefOrNull(v.category) &&
    isStr(v.receivedAt) && isStr(v.updatedAt) && isNum(v.ageSeconds) &&
    isObj(v.relatedWork) && isNum(v.relatedWork.open) && isNum(v.relatedWork.blockingOpen) &&
    isArr(v.allowedActions, isAction)
  );
}

const isBasis = (b: unknown) =>
  isObj(b) && b.unit === 'ticket' && b.dimensions === 'current_ticket_attributes' &&
  (b.resolved === null || b.resolved === 'resolve_event_in_range');
const isNumOrNull = (v: unknown) => v === null || isNum(v);

export function isTicketList(v: unknown): v is TicketList {
  return (
    isObj(v) && isArr(v.items, isTicketListItem) && isNum(v.total) &&
    isArr(v.countsByStatus, (c) => isObj(c) && isTicketStatus(c.status) && isNum(c.count)) &&
    isNumOrNull(v.resolutionEvents) && isStrOrNull(v.nextCursor) && isObj(v.filters) &&
    isBasis(v.basis) && isStr(v.asOf)
  );
}

export function isTicketBoard(v: unknown): v is TicketBoard {
  return (
    isObj(v) && isNum(v.total) && isNumOrNull(v.resolutionEvents) && isObj(v.filters) &&
    isBasis(v.basis) && isStr(v.asOf) &&
    isArr(v.columns, (c) => isObj(c) && isTicketStatus(c.status) && isNum(c.total) &&
      isArr(c.items, isTicketListItem) && isStrOrNull(c.nextCursor))
  );
}

export const isTicketMutationResult = (v: unknown): v is TicketMutationResult =>
  isObj(v) && isTicketListItem(v.ticket) && isStr(v.auditEventId);

export const isMemberList = (v: unknown): v is { items: Member[] } =>
  isObj(v) && isArr(v.items, (m) => isObj(m) && isStr(m.userId) && isStr(m.displayName) && typeof m.active === 'boolean');

const isCatalogItem = (c: unknown) =>
  isObj(c) && isStr(c.id) && isStr(c.code) && isStr(c.label) && typeof c.active === 'boolean' && isNum(c.sortOrder);
export const isCatalogs = (v: unknown): v is Catalogs =>
  isObj(v) && isStr(v.workspaceId) &&
  (['priorities', 'categories', 'intakeChannels'] as const).every((k) => isArr(v[k], isCatalogItem));
