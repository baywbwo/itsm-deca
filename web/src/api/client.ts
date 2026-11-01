import {
  isCatalogs,
  isDashboard,
  isErrorBody,
  isMemberList,
  isEmpty,
  isMeta,
  isSession,
  isSavedView,
  isSavedViewList,
  isSessionIssued,
  isTicketBoard,
  isTicketList,
  isTicketMutationResult,
  isWorkspace,
  isWorkspaceList,
  isWorkspacePermissions,
} from './guards';
import type {
  ApiErrorCode,
  Catalogs,
  Dashboard,
  DashboardPeriodKey,
  Member,
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
  Workspace,
  WorkspacePermissions,
} from './types';

export interface TransportRequest {
  method: 'GET' | 'POST' | 'PATCH' | 'DELETE';
  path: string;
  headers: Record<string, string>;
  signal?: AbortSignal | undefined;
  /** JSON request body (sent as application/json). */
  body?: unknown;
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
    readonly code: ApiErrorCode | 'network' | 'bad_response',
    message: string,
    readonly requestId: string | null = null,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

export const isAbort = (e: unknown) => e instanceof DOMException && e.name === 'AbortError';

/** Same-origin fetch; the session cookie is HttpOnly and never touched by script. */
export const fetchTransport: Transport = async ({ method, path, headers, signal, body: payload }) => {
  let res: Response;
  try {
    res = await fetch(path, {
      method,
      headers: {
        Accept: 'application/json',
        ...(payload === undefined ? {} : { 'Content-Type': 'application/json' }),
        ...headers,
      },
      ...(payload === undefined ? {} : { body: JSON.stringify(payload) }),
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

type Guard<T> = (v: unknown) => v is T;

export function createApi(transport: Transport) {
  // CSRF synchronizer token: memory only (never storage), returned once at entry.
  let csrfToken: string | null = null;

  async function call<T>(req: TransportRequest, guard: Guard<T>, ok = 200): Promise<T> {
    const res = await transport(req);
    if (res.status === ok && guard(res.body)) return res.body;
    if (isErrorBody(res.body)) {
      throw new ApiError(res.status, res.body.error.code, res.body.error.message, res.requestId);
    }
    if (res.status === ok) {
      throw new ApiError(0, 'bad_response', 'Respons server tidak sesuai kontrak.', res.requestId);
    }
    throw new ApiError(res.status, 'internal', 'Terjadi kesalahan pada server.', res.requestId);
  }

  const get = <T>(path: string, guard: Guard<T>, signal?: AbortSignal) =>
    call({ method: 'GET', path, headers: {}, signal }, guard);
  const ws = (id: string) => `/api/v1/workspaces/${encodeURIComponent(id)}`;
  const csrf = (): Record<string, string> => (csrfToken ? { 'X-CSRF-Token': csrfToken } : {});

  return {
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
