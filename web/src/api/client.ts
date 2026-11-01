import {
  isDashboard,
  isErrorBody,
  isMeta,
  isSession,
  isSessionIssued,
  isWorkspace,
  isWorkspaceList,
  isWorkspacePermissions,
} from './guards';
import type {
  ApiErrorCode,
  Dashboard,
  DashboardPeriodKey,
  Meta,
  Session,
  Workspace,
  WorkspacePermissions,
} from './types';

export interface TransportRequest {
  method: 'GET' | 'POST' | 'DELETE';
  path: string;
  headers: Record<string, string>;
  signal?: AbortSignal | undefined;
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
export const fetchTransport: Transport = async ({ method, path, headers, signal }) => {
  let res: Response;
  try {
    res = await fetch(path, {
      method,
      headers: { Accept: 'application/json', ...headers },
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
    /** Whole-scope aggregates for one workspace and period (server-side, not a page). */
    getDashboard: (id: string, period: DashboardPeriodKey, signal?: AbortSignal): Promise<Dashboard> =>
      get(`${ws(id)}/dashboard?period=${encodeURIComponent(period)}`, isDashboard, signal),
    /** Headers for unsafe methods (D06+ mutations). */
    csrfHeaders(): Record<string, string> {
      return csrfToken ? { 'X-CSRF-Token': csrfToken } : {};
    },
  };
}

export type Api = ReturnType<typeof createApi>;
