// Runtime checks at the network boundary. The compile-time types are generated; these
// only make sure an unexpected payload fails loudly instead of rendering garbage.
import type {
  ActivityPage,
  AttachmentUploaded,
  ConversationPanel,
  NoteCreated,
  NotePage,
  RelatedWorkCreated,
  ApiErrorBody,
  TicketDetail,
  TicketDetailMutation,
  AssignmentHistory,
  IntakeDirectory,
  IntegrationSync,
  OutboxRecoveryResult,
  TicketCreateResult,
  TicketIntegrations,
  Catalogs,
  Dashboard,
  Member,
  MemberList,
  MemberResult,
  WorkspaceSettings,
  SettingsBody,
  SettingsResult,
  CompanyList,
  CompanyResult, CompanyUpdateResult,
  CatalogEditor,
  CatalogEntry,
  CatalogEntryResult,
  IntegrationConnection,
  IntegrationList,
  JiraOptions,
  DirectorySyncResult,
  JiraUserList,
  AiSettingsView,
  AiProviderView,
  McpServerView,
  IntegrationResult,
  Meta,
  PermissionCode,
  SavedView,
  Session,
  SessionIssued,
  TicketBoard,
  TicketList,
  TicketListItem,
  TicketMutationResult,
  ReportPreview,
  ReportRun,
  ReportRunList,
  ReportExportResult,
  BrandKey,
  TemporaryPassword,
  UserCreated,
  UserDeactivated,
  UserList,
  UserListItem,
  HomeCompany,
  CompanyOptionList,
  UserCompanySet,
  Workspace,
  WorkspacePermissions,
  MfaStatus,
  MfaEnrollment,
  RecoveryCodes,
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
  if (v.accessMode !== 'uat_superadmin' && v.accessMode !== 'sso' && v.accessMode !== 'password') return false;
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
    isStrOrNull(v.banner) && typeof v.uatSuperadmin === 'boolean' &&
    // US-1.4. Absent in responses recorded before CP3a (review package): read as false; the
    // server still refuses every other route while the password must be changed.
    (v.mustChangePassword === undefined || typeof v.mustChangePassword === 'boolean') &&
    // CP3b: absent before CP3b (review package): read as false, so no admin page is offered.
    (v.platformAdmin === undefined || typeof v.platformAdmin === 'boolean')
  );
}

/** D11 CSRF re-issue after a reload. */
export const isCsrfIssued = (v: unknown): v is { csrfToken: string } =>
  isObj(v) && isStr(v.csrfToken) && v.csrfToken.length > 0;

export const isSessionIssued = (v: unknown): v is SessionIssued =>
  isSession(v) && isStr((v as Obj).csrfToken) && ((v as Obj).csrfToken as string).length > 0;

/** Only the bundled presets (or none): never a URL or a path. */
export const isBrandKey = (v: unknown): v is BrandKey => v === null || v === 'deca' || v === 'ajr' || v === 'bmt';

export function isWorkspace(v: unknown): v is Workspace {
  return (
    isObj(v) && isStr(v.id) && isStr(v.code) && isStr(v.name) && isStr(v.timezone) &&
    isStr(v.companyId) && typeof v.version === 'number' && isRole(v.myRole) &&
    isBrandKey(v.brandKey) && isStr(v.brandName)
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

const GRANTS = ['admin.members', 'admin.settings', 'integration.manage', 'integration.credentials', 'integration.recovery'];
const isGrant = (v: unknown) => isStr(v) && GRANTS.includes(v);
export const isMember = (m: unknown): m is Member =>
  isObj(m) && isStr(m.membershipId) && isStr(m.userId) && isStr(m.displayName) &&
  (m.role === 'cs' || m.role === 'lead_cs') && typeof m.active === 'boolean' && isNum(m.version) && isArr(m.grants, isGrant);
export const isMemberList = (v: unknown): v is MemberList =>
  isObj(v) && isArr(v.items, isMember) && isStrOrNull(v.myMembershipId) && typeof v.canManage === 'boolean' &&
  isArr(v.grantable, isGrant);

const isCatalogItem = (c: unknown) =>
  isObj(c) && isStr(c.id) && isStr(c.code) && isStr(c.label) && typeof c.active === 'boolean' && isNum(c.sortOrder);
export const isCatalogs = (v: unknown): v is Catalogs =>
  isObj(v) && isStr(v.workspaceId) &&
  (['priorities', 'categories', 'intakeChannels'] as const).every((k) => isArr(v[k], isCatalogItem));

export function isSavedView(v: unknown): v is SavedView {
  return (
    isObj(v) && isStr(v.id) && isStr(v.name) && (v.view === 'list' || v.view === 'board') &&
    isObj(v.query) && !('q' in v.query) && Object.values(v.query).every(isStr) &&
    isArr(v.columns, (c) => SAVED_VIEW_COLUMNS.includes(c as string)) && (v.columns as unknown[]).length > 0 &&
    isNum(v.version) && isStr(v.createdAt) && isStr(v.updatedAt)
  );
}
const SAVED_VIEW_COLUMNS = ['status', 'priority', 'owner', 'channel', 'received', 'updated'];
export const isSavedViewList = (v: unknown): v is { items: SavedView[] } =>
  isObj(v) && isArr(v.items, isSavedView);
/** 204 No Content. */
export const isEmpty = (v: unknown): v is null => v === null;

const SYNC_STATES = ['pending', 'success', 'failed', 'unknown'];
const isMode = (v: unknown) => v === 'simulation' || v === 'live';
export const isIntegrationSync = (v: unknown): v is IntegrationSync =>
  isObj(v) && isStr(v.id) && (v.provider === 'jira' || v.provider === 'lark') && isStr(v.operation) &&
  SYNC_STATES.includes(v.state as string) && isNum(v.attempt) && isStrOrNull(v.targetLabel ?? null) &&
  isStrOrNull(v.issueKey ?? null) && isStrOrNull(v.safeError ?? null) && typeof v.reconciled === 'boolean' &&
  isStr(v.updatedAt) && isMode(v.mode) && isStrOrNull(v.nextAttemptAt ?? null) &&
  isStrOrNull(v.issueUrl ?? null) &&
  isNum(v.autoAttempts) && typeof v.inFlight === 'boolean';

export const isTicketCreateResult = (v: unknown): v is TicketCreateResult =>
  isObj(v) && isTicketListItem(v.ticket) && typeof v.replayed === 'boolean' && isArr(v.integrations, isIntegrationSync);

export const isTicketIntegrations = (v: unknown): v is TicketIntegrations =>
  isObj(v) && isStr(v.ticketId) && isArr(v.items, isIntegrationSync) && typeof v.canRecover === 'boolean';

export const isOutboxRecoveryResult = (v: unknown): v is OutboxRecoveryResult =>
  isObj(v) && isIntegrationSync(v.item) && isStr(v.auditEventId);

const isEntry = (e: unknown) => isObj(e) && isStr(e.id) && isStr(e.displayName) && isStr(e.source);
const isProvider = (p: unknown) =>
  p === null || (isObj(p) && isStr(p.connectionId) && isStr(p.displayName) && isMode(p.mode) &&
    isStr(p.state) && isStr(p.simulatedOutcome) && isNum(p.version) && typeof p.usable === 'boolean' &&
    isArr(p.options, isEntry));
export const isIntakeDirectory = (v: unknown): v is IntakeDirectory =>
  isObj(v) && isStr(v.workspaceId) && isMode(v.integrationsMode) && isProvider(v.jira) && isProvider(v.lark) && isArr(v.merchants, isEntry);

const isPersonOrNull = (p: unknown) => p === undefined || p === null || (isObj(p) && isStr(p.id) && isStr(p.label));
export const isAssignmentHistory = (v: unknown): v is AssignmentHistory =>
  isObj(v) && isStr(v.ticketId) &&
  isArr(v.items, (e) => isObj(e) && isStr(e.id) && isStr(e.at) &&
    ['initial', 'take_for_self', 'reassign'].includes(e.kind as string) &&
    isPersonOrNull(e.actor) && isPersonOrNull(e.from) && isPersonOrNull(e.to) && isStrOrNull(e.reason ?? null));

// D08a ticket detail.
const isTextOrNull = isStrOrNull;
const isProps = (p: unknown) =>
  isObj(p) && isStr(p.subject) && isStr(p.chronology) && isStr(p.receivedAt) &&
  isRefOrNull(p.intakeChannel) && isRefOrNull(p.priority) && isRefOrNull(p.category) && isRefOrNull(p.product) &&
  isRefOrNull(p.receivingUnit) && isRefOrNull(p.region) && isRefOrNull(p.merchant) &&
  isTextOrNull(p.transactionActivity) && isTextOrNull(p.cause) && isTextOrNull(p.foreignBranchReference);
const isJiraOrNull = (j: unknown) =>
  j === null || (isObj(j) && isStr(j.issueKey) && isStrOrNull(j.externalStatus) && isStrOrNull(j.safeUrl));
const isRelatedWork = (r: unknown) =>
  isObj(r) && isStr(r.id) && isStr(r.title) && isStrOrNull(r.safeDescription) && typeof r.blocking === 'boolean' &&
  (r.completionState === 'open' || r.completionState === 'completed') && isStrOrNull(r.completionSummary) &&
  isStrOrNull(r.completionEvidence) && isRefOrNull(r.requestedBy) && isRefOrNull(r.completedBy) &&
  isStrOrNull(r.completedAt) && isJiraOrNull(r.jira) && isNum(r.version) && isStr(r.createdAt);
const isAttachmentMeta = (a: unknown) =>
  isObj(a) && isStr(a.id) && isStr(a.fileName) && isStrOrNull(a.inspectedMime) && isNum(a.sizeBytes) &&
  isStr(a.scanStatus) && (a.visibility === 'public' || a.visibility === 'internal') && isStr(a.createdAt);
const isResolution = (e: unknown) =>
  isObj(e) && isStr(e.id) && (e.action === 'resolve' || e.action === 'reopen') && isNum(e.cycleNo) &&
  isStr(e.summary) && isRefOrNull(e.actor) && isStr(e.at);
export const isTicketDetail = (v: unknown): v is TicketDetail =>
  isObj(v) && isTicketListItem(v.ticket) && isProps(v.properties) &&
  isObj(v.can) && typeof v.can.readOnly === 'boolean' && typeof v.can.editProperties === 'boolean' &&
  typeof v.can.manageRelatedWork === 'boolean' &&
  typeof v.can.addNote === 'boolean' && typeof v.can.uploadAttachment === 'boolean' &&
  isArr(v.relatedWork, isRelatedWork) && isArr(v.attachments, isAttachmentMeta) && isArr(v.resolutions, isResolution);
export const isTicketDetailMutation = (v: unknown): v is TicketDetailMutation =>
  isObj(v) && isTicketDetail(v.detail) && isStrOrNull(v.auditEventId);
export const isActivityPage = (v: unknown): v is ActivityPage =>
  isObj(v) && isStrOrNull(v.nextCursor) &&
  isArr(v.items, (i) => isObj(i) && isStr(i.id) && isStr(i.at) && isStr(i.action) && isStr(i.label) && isRefOrNull(i.actor));

// D08b follow-up: notes, read-only conversation panel, attachment upload, idempotent related work.
const isNote = (n: unknown) =>
  isObj(n) && isStr(n.id) && isStr(n.body) && isRefOrNull(n.author) && n.author !== null && isStr(n.createdAt);
export const isNotePage = (v: unknown): v is NotePage =>
  isObj(v) && isStrOrNull(v.nextCursor) && isArr(v.items, isNote);
export const isNoteCreated = (v: unknown): v is NoteCreated =>
  isObj(v) && isNote(v.note) && typeof v.replayed === 'boolean' && isStrOrNull(v.auditEventId);
const DIRECTIONS = ['inbound', 'outbound'];
const DELIVERY = ['received', 'queued', 'sent', 'delivered', 'failed', 'unknown'];
const isConversation = (c: unknown) =>
  isObj(c) && isStr(c.id) && c.source === 'simulation' && isStr(c.channelLabel) && isStr(c.externalThreadId) &&
  isArr(c.messages, (m) => isObj(m) && isStr(m.id) && DIRECTIONS.includes(m.direction as string) && isStr(m.body) &&
    DELIVERY.includes(m.deliveryState as string) && isStr(m.occurredAt));
export const isConversationPanel = (v: unknown): v is ConversationPanel =>
  isObj(v) && typeof v.adapterConfigured === 'boolean' && typeof v.truncated === 'boolean' && isArr(v.conversations, isConversation);
export const isRelatedWorkCreated = (v: unknown): v is RelatedWorkCreated =>
  isObj(v) && isTicketDetail(v.detail) && isStr(v.relatedWorkId) && typeof v.replayed === 'boolean' && isStrOrNull(v.auditEventId);
export const isAttachmentUploaded = (v: unknown): v is AttachmentUploaded =>
  isObj(v) && isStr(v.attachmentId) && isTicketDetail(v.detail) && isStr(v.auditEventId);

// D09 settings, members, catalogs and simulated integration connections.
const TIMEZONES = ['Asia/Jakarta', 'Asia/Makassar', 'Asia/Jayapura'];
const isWorkspaceSettings = (v: unknown): v is WorkspaceSettings =>
  isObj(v) && isStr(v.workspaceId) && isStr(v.workspaceCode) && isStr(v.workspaceName) && isStr(v.timezone) &&
  isStr(v.companyId) && isStr(v.companyCode) && isStr(v.companyName) && isStrOrNull(v.reporterCode) &&
  typeof v.active === 'boolean' && isNum(v.version) && isNum(v.memberCount) &&
  isBrandKey(v.brandKey) && isStrOrNull(v.brandName) && isArr(v.previousCodes, isStr);
export const isSettingsBody = (v: unknown): v is SettingsBody =>
  isObj(v) && isWorkspaceSettings(v.settings) && typeof v.canEdit === 'boolean' &&
  isArr(v.timezones, (t) => isStr(t) && TIMEZONES.includes(t));
export const isSettingsResult = (v: unknown): v is SettingsResult =>
  isObj(v) && isWorkspaceSettings(v.settings) && isStrOrNull(v.auditEventId);
export const isCompanyList = (v: unknown): v is CompanyList => isObj(v) && isArr(v.items, isWorkspaceSettings);
export const isCompanyResult = (v: unknown): v is CompanyResult =>
  isObj(v) && isWorkspaceSettings(v.item) && isStr(v.auditEventId);
export const isCompanyUpdateResult = (v: unknown): v is CompanyUpdateResult =>
  isObj(v) && isWorkspaceSettings(v.item) && isStrOrNull(v.auditEventId);
const CATALOG_KINDS = ['priorities', 'categories', 'products', 'receiving-units', 'regions', 'intake-channels'];
const isCatalogEntry = (v: unknown): v is CatalogEntry =>
  isObj(v) && isStr(v.id) && isStr(v.code) && isStr(v.label) && typeof v.active === 'boolean' &&
  isNum(v.sortOrder) && isNum(v.version);
export const isCatalogEditor = (v: unknown): v is CatalogEditor =>
  isObj(v) && isStr(v.kind) && CATALOG_KINDS.includes(v.kind) && isArr(v.items, isCatalogEntry) && typeof v.canEdit === 'boolean';
export const isCatalogEntryResult = (v: unknown): v is CatalogEntryResult =>
  isObj(v) && isStr(v.kind) && CATALOG_KINDS.includes(v.kind) && isCatalogEntry(v.item) && isStrOrNull(v.auditEventId);
export const isMemberResult = (v: unknown): v is MemberResult => isObj(v) && isMember(v.item) && isStr(v.auditEventId);
const isProviderCode = (v: unknown) => v === 'jira' || v === 'lark';
const CONNECTION_STATES = ['draft', 'tested', 'active', 'disabled'];
const isStrMap = (v: unknown) => isObj(v) && Object.values(v).every(isStr);
const isSettings = (v: unknown) => isObj(v) &&
  ['baseUrl', 'email', 'projectKey', 'issueTypeId', 'appId'].every((k) => isStrOrNull(v[k] ?? null)) &&
  isStrMap(v.priorityMap) && isStrMap(v.statusMap);
const isConnection = (v: unknown): v is IntegrationConnection =>
  isObj(v) && isStr(v.id) && isProviderCode(v.provider) && isStr(v.displayName) && isMode(v.mode) &&
  isStr(v.state) && CONNECTION_STATES.includes(v.state) && isStrOrNull(v.testedAt) && isStrOrNull(v.lastTestNote) &&
  (v.lastTestOk === null || typeof v.lastTestOk === 'boolean') &&
  isStr(v.simulatedOutcome) && isNum(v.version) && isNum(v.directoryCount) && isSettings(v.settings) &&
  isObj(v.credential) && (v.credential.state === 'missing' || v.credential.state === 'stored') && isStrOrNull(v.credential.updatedAt) &&
  isArr(v.recent, (r) => isObj(r) && isStr(r.action) && isStr(r.outcome) && isStr(r.occurredAt) && isStrOrNull(r.actor));
const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** D18: penyedia LLM — rahasia tidak pernah ada di payload, jadi tidak ada yang divalidasi di situ. */
export const isAiProviderView = (v: unknown): v is AiProviderView =>
  isRecord(v) && typeof v.provider === 'string' && typeof v.state === 'string' &&
  typeof v.keyPresent === 'boolean' && typeof v.version === 'number' && v.secret === 'env_only';

export const isMcpServerView = (v: unknown): v is McpServerView =>
  isRecord(v) && typeof v.id === 'string' && typeof v.name === 'string' &&
  (v.transport === 'http' || v.transport === 'stdio') && typeof v.enabled === 'boolean' &&
  typeof v.version === 'number' && typeof v.authPresent === 'boolean';

export const isAiSettingsView = (v: unknown): v is AiSettingsView =>
  isRecord(v) && Array.isArray(v.providers) && v.providers.every(isAiProviderView) &&
  Array.isArray(v.mcpServers) && v.mcpServers.every(isMcpServerView) && Array.isArray(v.notes);

export const isIntegrationList = (v: unknown): v is IntegrationList =>
  isObj(v) && isStr(v.workspaceId) && isArr(v.items, isConnection) && isArr(v.creatable, isProviderCode) &&
  typeof v.canManageCredentials === 'boolean' && isMode(v.integrationsMode) && typeof v.keyringConfigured === 'boolean' &&
  isArr(v.priorities, (p) => isObj(p) && isStr(p.code) && isStr(p.label)) && isArr(v.ticketStatuses, isStr);
const isIdName = (v: unknown) => isObj(v) && isStr(v.id) && isStr(v.name);
export const isJiraOptions = (v: unknown): v is JiraOptions =>
  isObj(v) && isArr(v.issueTypes, isIdName) && isArr(v.priorities, isIdName) && isArr(v.statuses, isIdName);
export const isDirectorySyncResult = (v: unknown): v is DirectorySyncResult =>
  isObj(v) && isConnection(v.item) && isStr(v.auditEventId) && isNum(v.synced) && isNum(v.deactivated);
export const isJiraUserList = (v: unknown): v is JiraUserList => isObj(v) && isArr(v.items, isEntry);
export const isIntegrationResult = (v: unknown): v is IntegrationResult =>
  isObj(v) && isConnection(v.item) && isStr(v.auditEventId);

// --- D10 reports -------------------------------------------------------------
const isReportBasis = (v: unknown) => v === 'received' || v === 'resolved';
const isBreakdown = (v: unknown) => isObj(v) && isStrOrNull(v.id) && isStrOrNull(v.label) && isNum(v.count);
const REPORT_TOTALS = [
  'tickets', 'resolutionCycles', 'resolvedTickets', 'unassigned', 'missingChannel', 'missingCategory', 'missingPriority',
] as const;
const LEVELS = ['info', 'warning', 'blocking'];

/** Preview of the operational report; totals must be numbers and the header must name the scope. */
export function isReportPreview(v: unknown): v is ReportPreview {
  if (!isObj(v) || v.reportType !== 'operational' || !isStr(v.reportLabel) || !isStr(v.asOf) || !isStr(v.notice)) return false;
  const h = v.header;
  if (!isObj(h) || !isStr(h.companyName) || !isStr(h.companyCode) || !isStrOrNull(h.reporterCode) ||
    !isStr(h.workspaceId) || !isStr(h.workspaceCode) || !isStr(h.timezone) || !isStr(h.periodStart) ||
    !isStr(h.periodEnd) || !isStr(h.startAt) || !isStr(h.endAt) || !isReportBasis(h.basis) || !isStr(h.basisLabel)) return false;
  const t = v.totals;
  const f = v.filters;
  const o = v.filterOptions;
  return (
    isObj(t) && REPORT_TOTALS.every((k) => isNum(t[k])) &&
    isObj(f) && FILTER_KEYS.every((k) => isStrOrNull(f[k])) && (f.status === null || isArr(f.status, isStr)) &&
    isObj(o) && (['owners', 'channels', 'categories', 'priorities'] as const).every((k) => isArr(o[k], isOption)) &&
    isArr(v.reportTypes, (r) => isObj(r) && isStr(r.code) && isStr(r.label) && typeof r.available === 'boolean' && isStrOrNull(r.reason)) &&
    isArr(v.byStatus, (s) => isObj(s) && isStatus(s.status) && isNum(s.count)) &&
    isArr(v.byPriority, isBreakdown) && isArr(v.byChannel, isBreakdown) &&
    isArr(v.byCategory, isBreakdown) && isArr(v.byOwner, isBreakdown) &&
    isArr(v.validation, (i) => isObj(i) && isStr(i.code) && LEVELS.includes(i.level as string) && isStr(i.message)) &&
    isArr(v.rows, (r) => isObj(r) && isStr(r.ticketId) && isStr(r.displayNumber) && isStr(r.subject) &&
      isStatus(r.status) && isStr(r.receivedLocal) && isNum(r.resolutionCycles)) &&
    isNum(v.previewLimit) && isNum(v.exportMaxRows) && typeof v.exportable === 'boolean'
  );
}

export function isReportRun(v: unknown): v is ReportRun {
  return (
    isObj(v) && isStr(v.id) && isStr(v.periodStart) && isStr(v.periodEnd) &&
    ['queued', 'running', 'succeeded', 'failed'].includes(v.state as string) &&
    isReportBasis(v.basis) && isObj(v.filters) && typeof v.downloadable === 'boolean' &&
    typeof v.hasArtifact === 'boolean' && typeof v.expired === 'boolean' &&
    isObj(v.requestedBy) && isStr(v.requestedBy.id) && isStr(v.requestedBy.label) &&
    isNumOrNull(v.rowCount) && isStrOrNull(v.artifactSha256) && isNumOrNull(v.artifactBytes) &&
    isStrOrNull(v.expiresAt) && isStrOrNull(v.failureCode) && isStr(v.createdAt) && isStrOrNull(v.fileName) &&
    (v.format === 'csv' || v.format === 'xlsx')
  );
}

export const isReportRunList = (v: unknown): v is ReportRunList =>
  isObj(v) && isArr(v.items, isReportRun) && isNum(v.limit);

export const isReportExportResult = (v: unknown): v is ReportExportResult =>
  isObj(v) && isReportRun(v.run) && typeof v.replayed === 'boolean';

// CP3b user administration. The temporary password must be a non-empty string; it is shown once.
const isBool = (v: unknown): v is boolean => typeof v === 'boolean';
const isTemp = (v: unknown) => isStr(v) && v.length > 0;

export const isUserListItem = (v: unknown): v is UserListItem =>
  isObj(v) && isStr(v.id) && isStrOrNull(v.username) && isStr(v.displayName) && isStrOrNull(v.email) &&
  isBool(v.active) && isBool(v.mustChangePassword) && isBool(v.locked) && isBool(v.platformAdmin) &&
  isBool(v.mfaEnrolled) && isStr(v.createdAt) && isStrOrNull(v.deactivatedAt) &&
  (v.company === null || isHomeCompany(v.company));

// CP5d: the user's one company (home membership) and the companies a platform admin can pick from.
export const isHomeCompany = (v: unknown): v is HomeCompany =>
  isObj(v) && isStr(v.workspaceId) && isStr(v.code) && isStr(v.name) && (v.role === 'cs' || v.role === 'lead_cs') &&
  isArr(v.grants, isGrant);

export const isCompanyOptionList = (v: unknown): v is CompanyOptionList =>
  isObj(v) && isArr(v.items, (c) => isObj(c) && isStr(c.workspaceId) && isStr(c.code) && isStr(c.name));

export const isUserCompanySet = (v: unknown): v is UserCompanySet =>
  isObj(v) && isUserListItem(v.user) && isBool(v.changed) && isStrOrNull(v.auditEventId);

export const isUserList = (v: unknown): v is UserList =>
  isObj(v) && isArr(v.items, isUserListItem) && isStrOrNull(v.nextCursor);

export const isUserCreated = (v: unknown): v is UserCreated =>
  isObj(v) && isTemp(v.temporaryPassword) && isObj(v.user) && isStr(v.user.id) && isStr(v.user.username) &&
  isStr(v.user.displayName) && isBool(v.user.active) && isBool(v.user.mustChangePassword);

export const isTemporaryPassword = (v: unknown): v is TemporaryPassword => isObj(v) && isTemp(v.temporaryPassword);

export const isUserDeactivated = (v: unknown): v is UserDeactivated => isObj(v) && isStr(v.id) && v.active === false;

// CP4c (US-1.2): MFA state, enrolment and one-time recovery codes. Shapes only; values are never logged.
export const isMfaStatus = (v: unknown): v is MfaStatus =>
  isObj(v) && isBool(v.enrolled) && isBool(v.required) && isNum(v.recoveryCodesLeft);

export const isMfaEnrollment = (v: unknown): v is MfaEnrollment =>
  isObj(v) && isStr(v.enrollmentId) && isStr(v.otpauthUri) && v.otpauthUri.startsWith('otpauth://totp/') && isStr(v.expiresAt);

export const isRecoveryCodes = (v: unknown): v is RecoveryCodes =>
  isObj(v) && isArr(v.recoveryCodes, isStr) && (v.recoveryCodes as unknown[]).length === 10;
