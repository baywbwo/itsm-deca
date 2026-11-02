import {
  isActivityPage,
  isAttachmentUploaded,
  isConversationPanel,
  isNoteCreated,
  isNotePage,
  isRelatedWorkCreated,
  isTicketDetail,
  isTicketDetailMutation,
  isCatalogs,
  isDashboard,
  isErrorBody,
  isMemberList,
  isMemberResult,
  isSettingsBody,
  isSettingsResult,
  isCompanyList,
  isCompanyResult,
  isCompanyUpdateResult,
  isCatalogEditor,
  isCatalogEntryResult,
  isIntegrationList,
  isIntegrationResult,
  isEmpty,
  isAssignmentHistory,
  isIntakeDirectory,
  isOutboxRecoveryResult,
  isTicketCreateResult,
  isTicketIntegrations,
  isMeta,
  isSession,
  isSavedView,
  isSavedViewList,
  isSessionIssued,
  isCsrfIssued,
  isTicketBoard,
  isTicketList,
  isTicketMutationResult,
  isWorkspace,
  isWorkspaceList,
  isReportPreview,
  isReportRunList,
  isReportRun,
  isReportExportResult,
  isWorkspacePermissions,
} from './guards';
import type {
  ActivityPage,
  AttachmentUploaded,
  ConversationPanel,
  NoteCreateRequest,
  NoteCreated,
  NotePage,
  RelatedWorkCreated,
  RelatedWorkCompletionRequest,
  RelatedWorkCreateRequest,
  TicketDetail,
  TicketDetailMutation,
  TicketPropertiesRequest,
  ApiErrorCode,
  AssignmentHistory,
  IntakeDirectory,
  OutboxReconcileRequest,
  OutboxRecoveryResult,
  OutboxRetryRequest,
  TicketCreateRequest,
  TicketCreateResult,
  TicketIntegrations,
  Catalogs,
  Dashboard,
  DashboardPeriodKey,
  Member,
  MemberList,
  MemberCreateRequest,
  MemberPatchRequest,
  GrantChangeRequest,
  MemberResult,
  SettingsBody,
  SettingsPatchRequest,
  CompanyProfilePatchRequest,
  SettingsResult,
  CompanyList,
  CompanyCreateRequest,
  CompanyActivationRequest,
  CompanyResult,
  CompanyUpdateResult,
  CatalogKind,
  CatalogEditor,
  CatalogCreateRequest,
  CatalogPatchRequest,
  CatalogEntryResult,
  IntegrationList,
  IntegrationCreateRequest,
  IntegrationPatchRequest,
  IntegrationResult,
  VersionRequest,
  Meta,
  SavedView,
  SavedViewCreateRequest,
  SavedViewRenameRequest,
  Session,
  TicketAssignmentRequest,
  TicketBoard,
  TicketList,
  TicketMutationResult,
  TicketTransitionRequest,
  ReportPreview,
  ReportRun,
  ReportRunList,
  ReportExportRequest,
  ReportExportResult,
  Workspace,
  WorkspacePermissions,
} from './types';

export interface TransportRequest {
  method: 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE';
  path: string;
  headers: Record<string, string>;
  signal?: AbortSignal | undefined;
  /** JSON request body (sent as application/json). */
  body?: unknown;
  /** Raw file body (attachment upload, application/octet-stream); excludes `body`. */
  file?: Blob;
}
export interface TransportResponse {
  status: number;
  body: unknown;
  requestId: string | null;
}
export type Transport = (req: TransportRequest) => Promise<TransportResponse>;

/** Failure with a stable code; `status` 0 means the network or the payload shape failed. */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: ApiErrorCode | 'network' | 'bad_response' | 'timeout',
    message: string,
    readonly requestId: string | null = null,
    /** validation_failed only: the request field to fix. */
    readonly field: string | null = null,
    /** rate_limited only: seconds until the server accepts the next request (Retry-After). */
    readonly retryAfterSeconds: number | null = null,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

/**
 * 429 copy: when to try again. A rate-limited request was not processed, so the caller keeps the
 * input and leaves the retry to the person (mutations are never re-sent automatically).
 */
export function rateLimitText(e: ApiError): string {
  const n = e.retryAfterSeconds;
  return n !== null && n > 0
    ? `Terlalu banyak permintaan dalam waktu singkat. Coba lagi dalam ${n} detik.`
    : 'Terlalu banyak permintaan dalam waktu singkat. Coba lagi sebentar lagi.';
}

/**
 * 403 csrf_failed that is still refused after the single token refresh in `send`: the session's
 * CSRF token could not be renewed in this tab. Not a permission refusal; the request was refused
 * before any handler ran, so nothing changed and the caller keeps the input.
 */
export const isSessionReloadNeeded = (e: unknown): e is ApiError & { code: 'csrf_failed' } =>
  e instanceof ApiError && e.status === 403 && e.code === 'csrf_failed';
export const SESSION_RELOAD_TITLE = 'Sesi perlu dimuat ulang';
export function sessionReloadText(requestId: string | null): string {
  const rid = requestId ? ` (ID permintaan ${requestId})` : '';
  return `Token keamanan sesi di tab ini tidak dapat diperbarui otomatis. Permintaan belum diproses dan isian Anda tetap di sini. Muat ulang halaman (salin isian dulu bila perlu), lalu kirim lagi.${rid}`;
}

export const isAbort = (e: unknown) => e instanceof DOMException && e.name === 'AbortError';

/** Same-origin fetch; the session cookie is HttpOnly and never touched by script. */
export const fetchTransport: Transport = async ({ method, path, headers, signal, body: payload, file }) => {
  let res: Response;
  try {
    res = await fetch(path, {
      method,
      headers: {
        Accept: 'application/json',
        ...(payload === undefined ? {} : { 'Content-Type': 'application/json' }),
        ...(file === undefined ? {} : { 'Content-Type': 'application/octet-stream' }),
        ...headers,
      },
      ...(payload === undefined ? {} : { body: JSON.stringify(payload) }),
      ...(file === undefined ? {} : { body: file }),
      credentials: 'same-origin',
      cache: 'no-store',
      signal: signal ?? null,
    });
  } catch (e) {
    if (isAbort(e)) throw e;
    throw new ApiError(0, 'network', 'Server tidak dapat dihubungi.');
  }
  const text = await res.text();
  let body: unknown = null;
  if (text) {
    try {
      body = JSON.parse(text);
    } catch {
      body = null;
    }
  }
  return { status: res.status, body, requestId: res.headers.get('x-request-id') };
};

/** Rejects with an AbortError as soon as `signal` aborts, without cancelling the shared work. */
function untilAbort<T>(p: Promise<T>, signal?: AbortSignal): Promise<T> {
  if (!signal) return p;
  return new Promise<T>((resolve, reject) => {
    const abort = () => reject(new DOMException('Aborted', 'AbortError'));
    if (signal.aborted) return abort();
    signal.addEventListener('abort', abort, { once: true });
    p.then(
      (v) => { signal.removeEventListener('abort', abort); resolve(v); },
      (e: unknown) => { signal.removeEventListener('abort', abort); reject(e instanceof Error ? e : new Error(String(e))); },
    );
  });
}

type Guard<T> = (v: unknown) => v is T;

export function createApi(transport: Transport) {
  // CSRF synchronizer token: memory only (never storage), returned once at entry.
  // D11: a reloaded tab keeps the HttpOnly cookie but not this token; it is re-issued from the
  // session (same origin, custom header) before the first unsafe request, and once more if the
  // server refuses it (another tab of the same session rotated it). A CSRF refusal happens before
  // any handler runs, so the single retry cannot apply a change twice.
  // A 429 is returned to the caller as is: never re-sent here (a person decides when to retry).
  let csrfToken: string | null = null;
  let refreshing: Promise<string> | null = null;

  const CSRF_EXEMPT = new Set(['/api/v1/uat/session', '/api/v1/session/csrf', '/api/v1/login']);

  // Shared by concurrent callers; each caller still stops at its own abort or timeout.
  function refreshCsrf(signal?: AbortSignal): Promise<string> {
    refreshing ??= (async () => {
      const res = await transport({
        method: 'POST', path: '/api/v1/session/csrf', headers: { 'X-ITSM-CSRF-Refresh': '1' },
        signal: AbortSignal.timeout(10_000),
      });
      if (res.status !== 200 || !isCsrfIssued(res.body)) {
        const wait = isErrorBody(res.body) ? (res.body.error as { retryAfterSeconds?: unknown }).retryAfterSeconds : null;
        throw isErrorBody(res.body)
          ? new ApiError(res.status, res.body.error.code, res.body.error.message, res.requestId, null,
            typeof wait === 'number' && Number.isFinite(wait) ? wait : null)
          : new ApiError(res.status, 'internal', 'Terjadi kesalahan pada server.', res.requestId);
      }
      csrfToken = res.body.csrfToken;
      return csrfToken;
    })().finally(() => { refreshing = null; });
    return untilAbort(refreshing, signal);
  }

  async function send(req: TransportRequest): Promise<TransportResponse> {
    if (req.method === 'GET' || CSRF_EXEMPT.has(req.path)) return transport(req);
    const withToken = (t: string) => ({ ...req, headers: { ...req.headers, 'X-CSRF-Token': t } });
    const res = await transport(withToken(csrfToken ?? (await refreshCsrf(req.signal))));
    const refused = res.status === 403 && isErrorBody(res.body) && res.body.error.code === 'csrf_failed';
    return refused ? transport(withToken(await refreshCsrf(req.signal))) : res;
  }

  async function call<T>(req: TransportRequest, guard: Guard<T>, ok: number | number[] = 200): Promise<T> {
    const res = await send(req);
    const okStatus = Array.isArray(ok) ? ok.includes(res.status) : res.status === ok;
    if (okStatus && guard(res.body)) return res.body;
    if (isErrorBody(res.body)) {
      const { field, retryAfterSeconds: wait } = res.body.error as { field?: unknown; retryAfterSeconds?: unknown };
      throw new ApiError(res.status, res.body.error.code, res.body.error.message, res.requestId,
        typeof field === 'string' ? field : null,
        typeof wait === 'number' && Number.isFinite(wait) ? wait : null);
    }
    if (okStatus) {
      throw new ApiError(0, 'bad_response', 'Respons server tidak sesuai kontrak.', res.requestId);
    }
    throw new ApiError(res.status, 'internal', 'Terjadi kesalahan pada server.', res.requestId);
  }

  const get = <T>(path: string, guard: Guard<T>, signal?: AbortSignal) =>
    call({ method: 'GET', path, headers: {}, signal }, guard);
  const ws = (id: string) => `/api/v1/workspaces/${encodeURIComponent(id)}`;
  const csrf = (): Record<string, string> => (csrfToken ? { 'X-CSRF-Token': csrfToken } : {});

  return {
    suggestWording: (workspaceId: string, mode: 'chronology' | 'reply', text: string): Promise<{ suggestion: string; reviewRequired: boolean }> =>
      call({ method: 'POST', path: `${ws(workspaceId)}/ai-suggestions`, headers: csrf(), body: { mode, text } },
        (value: unknown): value is { suggestion: string; reviewRequired: boolean } => !!value && typeof value === 'object' &&
          'suggestion' in value && typeof value.suggestion === 'string' && 'reviewRequired' in value && value.reviewRequired === true),
    async login(username: string, password: string): Promise<Session> {
      const issued = await call({ method: 'POST', path: '/api/v1/login', headers: { 'X-ITSM-Login': '1' }, body: { username, password } }, isSessionIssued, 201);
      csrfToken = issued.csrfToken;
      return { user: issued.user, banner: issued.banner, uatSuperadmin: issued.uatSuperadmin };
    },
    async logout(): Promise<void> {
      await call({ method: 'DELETE', path: '/api/v1/session', headers: csrf() }, isEmpty, 204);
      csrfToken = null;
    },
    hasCsrf: () => csrfToken !== null,
    getMeta: (signal?: AbortSignal): Promise<Meta> => get('/api/v1/meta', isMeta, signal),
    getSession: (signal?: AbortSignal): Promise<Session> => get('/api/v1/session', isSession, signal),
    /** Direct UAT Superadmin entry; the server refuses it outside UAT on a marked dummy DB. */
    async enterUat(signal?: AbortSignal): Promise<Session> {
      const issued = await call(
        { method: 'POST', path: '/api/v1/uat/session', headers: { 'X-ITSM-UAT-Entry': '1' }, signal },
        isSessionIssued,
        201,
      );
      csrfToken = issued.csrfToken;
      return { user: issued.user, banner: issued.banner, uatSuperadmin: issued.uatSuperadmin };
    },
    listWorkspaces: async (signal?: AbortSignal): Promise<Workspace[]> =>
      (await get('/api/v1/workspaces', isWorkspaceList, signal)).items,
    getWorkspace: (id: string, signal?: AbortSignal): Promise<Workspace> =>
      get(ws(id), isWorkspace, signal),
    getMyPermissions: (id: string, signal?: AbortSignal): Promise<WorkspacePermissions> =>
      get(`${ws(id)}/permissions`, isWorkspacePermissions, signal),
    /**
     * Whole-scope aggregates for one workspace, period and shared filters (server-side, not a
     * page). `query` is the canonical dashboard query (`dashboardQuery`) or a bare period key.
     */
    getDashboard: (id: string, query: DashboardPeriodKey | string, signal?: AbortSignal): Promise<Dashboard> =>
      get(`${ws(id)}/dashboard?${query.includes('=') ? query : `period=${encodeURIComponent(query)}`}`, isDashboard, signal),
    /**
     * One page of the ticket List. `query` is the canonical list query (`ticketQuery`); `cursor`
     * continues the same scope/filter/sort (the server refuses a cursor from another one).
     */
    listTickets: (id: string, query: string, cursor: string | null, signal?: AbortSignal): Promise<TicketList> =>
      get(`${ws(id)}/tickets${withParam(query, 'cursor', cursor)}`, isTicketList, signal),
    /** Board columns (one per status in scope) with server totals and a first page each. */
    getBoard: (id: string, query: string, signal?: AbortSignal): Promise<TicketBoard> =>
      get(`${ws(id)}/tickets/board${query ? `?${query}` : ''}`, isTicketBoard, signal),
    /** Lifecycle move/resolve/reopen; the server re-checks permission, guard and version. */
    transitionTicket: (id: string, ticketId: string, body: TicketTransitionRequest): Promise<TicketMutationResult> =>
      call({
        method: 'POST', path: `${ws(id)}/tickets/${encodeURIComponent(ticketId)}/transitions`,
        headers: csrf(), body,
      }, isTicketMutationResult),
    assignTicket: (id: string, ticketId: string, body: TicketAssignmentRequest): Promise<TicketMutationResult> =>
      call({
        method: 'POST', path: `${ws(id)}/tickets/${encodeURIComponent(ticketId)}/assignment`,
        headers: csrf(), body,
      }, isTicketMutationResult),
    listMembers: async (id: string, signal?: AbortSignal): Promise<Member[]> =>
      (await get(`${ws(id)}/members`, isMemberList, signal)).items,
    getCatalogs: (id: string, signal?: AbortSignal): Promise<Catalogs> =>
      get(`${ws(id)}/catalogs`, isCatalogs, signal),
    /** C32: the caller's own saved views in this workspace (private, no PII, grants nothing). */
    listSavedViews: async (id: string, signal?: AbortSignal): Promise<SavedView[]> =>
      (await get(`${ws(id)}/saved-views`, isSavedViewList, signal)).items,
    createSavedView: (id: string, body: SavedViewCreateRequest): Promise<SavedView> =>
      call({ method: 'POST', path: `${ws(id)}/saved-views`, headers: csrf(), body }, isSavedView, 201),
    renameSavedView: (id: string, viewId: string, body: SavedViewRenameRequest): Promise<SavedView> =>
      call({
        method: 'PATCH', path: `${ws(id)}/saved-views/${encodeURIComponent(viewId)}`,
        headers: csrf(), body,
      }, isSavedView),
    deleteSavedView: async (id: string, viewId: string): Promise<void> => {
      await call({
        method: 'DELETE', path: `${ws(id)}/saved-views/${encodeURIComponent(viewId)}`,
        headers: csrf(),
      }, isEmpty, 204);
    },
    /**
     * D07 intake. `key` is the draft's idempotency key: resend the SAME key and payload after a
     * timeout or network error; the server replays the original ticket instead of creating a
     * second one. Times out client-side after `timeoutMs` (outcome then unknown, code timeout).
     */
    async createTicket(id: string, key: string, body: TicketCreateRequest, timeoutMs = 15000): Promise<TicketCreateResult> {
      const ctrl = new AbortController();
      const timer = setTimeout(() => ctrl.abort(), timeoutMs);
      try {
        return await call({
          method: 'POST', path: `${ws(id)}/tickets`, headers: { ...csrf(), 'Idempotency-Key': key },
          body, signal: ctrl.signal,
        }, isTicketCreateResult, [200, 201]);
      } catch (e) {
        if (isAbort(e)) throw new ApiError(0, 'timeout', 'Server belum menjawab. Hasil pengiriman belum diketahui.');
        throw e;
      } finally {
        clearTimeout(timer);
      }
    },
    getIntakeDirectory: (id: string, signal?: AbortSignal): Promise<IntakeDirectory> =>
      get(`${ws(id)}/intake-directory`, isIntakeDirectory, signal),
    getAssignmentHistory: (id: string, ticketId: string, signal?: AbortSignal): Promise<AssignmentHistory> =>
      get(`${ws(id)}/tickets/${encodeURIComponent(ticketId)}/assignment-history`, isAssignmentHistory, signal),
    getTicketIntegrations: (id: string, ticketId: string, signal?: AbortSignal): Promise<TicketIntegrations> =>
      get(`${ws(id)}/tickets/${encodeURIComponent(ticketId)}/integrations`, isTicketIntegrations, signal),
    retryOutbox: (id: string, outboxId: string, body: OutboxRetryRequest): Promise<OutboxRecoveryResult> =>
      call({
        method: 'POST', path: `${ws(id)}/integration-outbox/${encodeURIComponent(outboxId)}/retry`,
        headers: csrf(), body,
      }, isOutboxRecoveryResult),
    reconcileOutbox: (id: string, outboxId: string, body: OutboxReconcileRequest): Promise<OutboxRecoveryResult> =>
      call({
        method: 'POST', path: `${ws(id)}/integration-outbox/${encodeURIComponent(outboxId)}/reconcile`,
        headers: csrf(), body,
      }, isOutboxRecoveryResult),
    /** D08a: one ticket with its properties, related work, attachment metadata and resolutions. */
    getTicketDetail: (id: string, ticketId: string, signal?: AbortSignal): Promise<TicketDetail> =>
      get(`${ws(id)}/tickets/${encodeURIComponent(ticketId)}/detail`, isTicketDetail, signal),
    /** Safe activity labels from the audit log, newest first; `cursor` continues the same ticket. */
    listTicketActivity: (id: string, ticketId: string, cursor: string | null, signal?: AbortSignal): Promise<ActivityPage> =>
      get(`${ws(id)}/tickets/${encodeURIComponent(ticketId)}/activity${withParam('', 'cursor', cursor)}`, isActivityPage, signal),
    /** Only the fields sent change; `null` clears. The version guards a concurrent edit (409). */
    updateTicketProperties: (id: string, ticketId: string, body: TicketPropertiesRequest): Promise<TicketDetailMutation> =>
      call({
        method: 'PATCH', path: `${ws(id)}/tickets/${encodeURIComponent(ticketId)}`, headers: csrf(), body,
      }, isTicketDetailMutation),
    /** Idempotent per key: 201 new, 200 replay of the same payload, 409 when the key carried another payload. */
    createRelatedWork: (id: string, ticketId: string, key: string, body: RelatedWorkCreateRequest): Promise<RelatedWorkCreated> =>
      call({
        method: 'POST', path: `${ws(id)}/tickets/${encodeURIComponent(ticketId)}/related-work`,
        headers: { ...csrf(), 'Idempotency-Key': key }, body,
      }, isRelatedWorkCreated, [200, 201]),
    /** D08b operational follow-up notes, newest first. */
    listTicketNotes: (id: string, ticketId: string, cursor: string | null, signal?: AbortSignal): Promise<NotePage> =>
      get(`${ws(id)}/tickets/${encodeURIComponent(ticketId)}/notes${withParam('', 'cursor', cursor)}`, isNotePage, signal),
    createTicketNote: (id: string, ticketId: string, key: string, body: NoteCreateRequest): Promise<NoteCreated> =>
      call({
        method: 'POST', path: `${ws(id)}/tickets/${encodeURIComponent(ticketId)}/notes`,
        headers: { ...csrf(), 'Idempotency-Key': key }, body,
      }, isNoteCreated, [200, 201]),
    /** Read-only; no omnichannel adapter exists, so nothing can be sent from here. */
    getTicketConversations: (id: string, ticketId: string, signal?: AbortSignal): Promise<ConversationPanel> =>
      get(`${ws(id)}/tickets/${encodeURIComponent(ticketId)}/conversations`, isConversationPanel, signal),
    /** Quarantine -> scan -> bind on the server; only a clean scan binds. Fails closed otherwise. */
    uploadTicketAttachment: (id: string, ticketId: string, file: File, visibility: 'internal' | 'public'): Promise<AttachmentUploaded> =>
      call({
        method: 'POST',
        path: `${ws(id)}/tickets/${encodeURIComponent(ticketId)}/attachments?visibility=${visibility}`,
        headers: { ...csrf(), 'X-File-Name': encodeURIComponent(file.name) }, file,
      }, isAttachmentUploaded, 201),
    /** Same-origin link; the server re-checks scope, scan state and integrity on every download. */
    attachmentContentUrl: (id: string, ticketId: string, attachmentId: string): string =>
      `${ws(id)}/tickets/${encodeURIComponent(ticketId)}/attachments/${encodeURIComponent(attachmentId)}/content`,
    /** A person completes related work with summary + evidence; a Jira status never does. */
    completeRelatedWork: (id: string, ticketId: string, workId: string, body: RelatedWorkCompletionRequest): Promise<TicketDetailMutation> =>
      call({
        method: 'POST',
        path: `${ws(id)}/tickets/${encodeURIComponent(ticketId)}/related-work/${encodeURIComponent(workId)}/completion`,
        headers: csrf(), body,
      }, isTicketDetailMutation),
    /** D10: server-side preview of the whole scope; `query` is the canonical report query (`reportApiQuery`). */
    previewReport: (id: string, query: string, signal?: AbortSignal): Promise<ReportPreview> =>
      get(`${ws(id)}/reports/preview?${query}`, isReportPreview, signal),
    listReportRuns: (id: string, signal?: AbortSignal): Promise<ReportRunList> =>
      get(`${ws(id)}/report-runs`, isReportRunList, signal),
    getReportRun: (id: string, runId: string, signal?: AbortSignal): Promise<ReportRun> =>
      get(`${ws(id)}/report-runs/${encodeURIComponent(runId)}`, isReportRun, signal),
    /** Idempotent per key: 201 new run, 200 replay of the same spec, 409 when the key carried another spec. */
    createReportRun: (id: string, key: string, body: ReportExportRequest): Promise<ReportExportResult> =>
      call({
        method: 'POST', path: `${ws(id)}/report-runs`, headers: { ...csrf(), 'Idempotency-Key': key }, body,
      }, isReportExportResult, [200, 201]),
    /** Same-origin link; the server re-checks scope, expiry and the stored hash on every download. */
    reportContentUrl: (id: string, runId: string): string =>
      `${ws(id)}/report-runs/${encodeURIComponent(runId)}/content`,
    /** D09: workspace profile; `canEdit` mirrors admin.settings (the server re-checks every write). */
    getSettings: (id: string, signal?: AbortSignal): Promise<SettingsBody> =>
      get(`${ws(id)}/settings`, isSettingsBody, signal),
    updateSettings: (id: string, body: SettingsPatchRequest): Promise<SettingsResult> =>
      call({ method: 'PATCH', path: `${ws(id)}/settings`, headers: csrf(), body }, isSettingsResult),
    /** UAT Superadmin only (403 otherwise): one company = one workspace, created without members. */
    listCompanies: (signal?: AbortSignal): Promise<CompanyList> => get('/api/v1/companies', isCompanyList, signal),
    createCompany: (body: CompanyCreateRequest): Promise<CompanyResult> =>
      call({ method: 'POST', path: '/api/v1/companies', headers: csrf(), body }, isCompanyResult, 201),
    setCompanyActive: (companyId: string, body: CompanyActivationRequest): Promise<CompanyResult> =>
      call({ method: 'PATCH', path: `/api/v1/companies/${encodeURIComponent(companyId)}`, headers: csrf(), body }, isCompanyResult),
    /** Edits names/time zone/reporter code; the code is immutable. `auditEventId` is null when nothing changed. */
    updateCompany: (companyId: string, body: CompanyProfilePatchRequest): Promise<CompanyUpdateResult> =>
      call({ method: 'PATCH', path: `/api/v1/companies/${encodeURIComponent(companyId)}/profile`, headers: csrf(), body }, isCompanyUpdateResult),
    listCatalogEntries: (id: string, kind: CatalogKind, signal?: AbortSignal): Promise<CatalogEditor> =>
      get(`${ws(id)}/catalogs/${kind}`, isCatalogEditor, signal),
    createCatalogEntry: (id: string, kind: CatalogKind, body: CatalogCreateRequest): Promise<CatalogEntryResult> =>
      call({ method: 'POST', path: `${ws(id)}/catalogs/${kind}`, headers: csrf(), body }, isCatalogEntryResult, 201),
    updateCatalogEntry: (id: string, kind: CatalogKind, itemId: string, body: CatalogPatchRequest): Promise<CatalogEntryResult> =>
      call({
        method: 'PATCH', path: `${ws(id)}/catalogs/${kind}/${encodeURIComponent(itemId)}`, headers: csrf(), body,
      }, isCatalogEntryResult),
    /** Members with `canManage` and the grants the caller may hand out (only ones it holds). */
    getMemberList: (id: string, signal?: AbortSignal): Promise<MemberList> =>
      get(`${ws(id)}/members`, isMemberList, signal),
    addMember: (id: string, body: MemberCreateRequest): Promise<MemberResult> =>
      call({ method: 'POST', path: `${ws(id)}/members`, headers: csrf(), body }, isMemberResult, 201),
    updateMember: (id: string, membershipId: string, body: MemberPatchRequest): Promise<MemberResult> =>
      call({
        method: 'PATCH', path: `${ws(id)}/members/${encodeURIComponent(membershipId)}`, headers: csrf(), body,
      }, isMemberResult),
    addMemberGrant: (id: string, membershipId: string, body: GrantChangeRequest): Promise<MemberResult> =>
      call({
        method: 'POST', path: `${ws(id)}/members/${encodeURIComponent(membershipId)}/grants`, headers: csrf(), body,
      }, isMemberResult, 201),
    revokeMemberGrant: (id: string, membershipId: string, body: GrantChangeRequest): Promise<MemberResult> =>
      call({
        method: 'POST', path: `${ws(id)}/members/${encodeURIComponent(membershipId)}/grants/revoke`, headers: csrf(), body,
      }, isMemberResult),
    /** Simulated connections only: no provider is contacted and no credential exists. */
    listIntegrations: (id: string, signal?: AbortSignal): Promise<IntegrationList> =>
      get(`${ws(id)}/integrations`, isIntegrationList, signal),
    createIntegration: (id: string, body: IntegrationCreateRequest): Promise<IntegrationResult> =>
      call({ method: 'POST', path: `${ws(id)}/integrations`, headers: csrf(), body }, isIntegrationResult, 201),
    updateIntegration: (id: string, connectionId: string, body: IntegrationPatchRequest): Promise<IntegrationResult> =>
      call({
        method: 'PATCH', path: `${ws(id)}/integrations/${encodeURIComponent(connectionId)}`, headers: csrf(), body,
      }, isIntegrationResult),
    /** Save → test → activate; disable from any state. */
    stepIntegration: (id: string, connectionId: string, step: 'test' | 'activate' | 'disable', body: VersionRequest): Promise<IntegrationResult> =>
      call({
        method: 'POST', path: `${ws(id)}/integrations/${encodeURIComponent(connectionId)}/${step}`, headers: csrf(), body,
      }, isIntegrationResult),
    /** Headers for unsafe methods (D06+ mutations). */
    csrfHeaders: csrf,
  };
}

export type Api = ReturnType<typeof createApi>;

/** `query` plus one more parameter (skipped when `value` is null). */
function withParam(query: string, key: string, value: string | null): string {
  const q = new URLSearchParams(query);
  if (value !== null) q.set(key, value);
  const s = q.toString();
  return s ? `?${s}` : '';
}
