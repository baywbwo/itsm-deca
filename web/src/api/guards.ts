// Runtime checks at the network boundary. The compile-time types are generated; these
// only make sure an unexpected payload fails loudly instead of rendering garbage.
import type {
  ApiErrorBody,
  Dashboard,
  Meta,
  PermissionCode,
  Session,
  SessionIssued,
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

/** Shape check of the dashboard aggregate; numbers must be numbers, lists must be lists. */
export function isDashboard(v: unknown): v is Dashboard {
  if (!isObj(v) || !isStr(v.workspaceId) || !isStr(v.workspaceCode) || !isStr(v.asOf)) return false;
  const p = v.period;
  if (!isObj(p) || !PERIODS.includes(p.key as string) || !isStr(p.timezone) || !isStr(p.startDate) ||
    !isStr(p.endDate) || !isStr(p.startAt) || !isStr(p.endAt)) return false;
  const t = v.totals;
  if (!isObj(t) || !TOTALS.every((k) => isNum(t[k]))) return false;
  const a = v.attention;
  return (
    typeof v.slaConfigured === 'boolean' && isObj(v.drilldown) &&
    isArr(v.statusDistribution, (s) => isObj(s) && isStatus(s.status) && isNum(s.count) && isFilter(s.filter)) &&
    isArr(v.picLoad, (r) => isObj(r) && isStrOrNull(r.userId) && isStrOrNull(r.displayName) && isNum(r.activeTickets)) &&
    isArr(v.channels, (c) => isObj(c) && isStrOrNull(c.label) && isNum(c.count)) &&
    isArr(v.age, (b) => isObj(b) && isStr(b.label) && isNum(b.count)) &&
    isArr(v.trend, (d) => isObj(d) && isStr(d.date) && isNum(d.received) && isNum(d.resolved)) &&
    isObj(a) && isNum(a.total) &&
    isArr(a.items, (i) => isObj(i) && isStr(i.id) && isStr(i.displayNumber) && isStr(i.subject) && isStatus(i.status) && isNum(i.ageSeconds))
  );
}
