// Runtime checks at the network boundary. The compile-time types are generated; these
// only make sure an unexpected payload fails loudly instead of rendering garbage.
import type {
  ApiErrorBody,
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
