import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { ApiError, isAbort, type Api } from '../api/client';
import type {
  CatalogItem,
  Dashboard,
  Member,
  Meta,
  Session,
  TicketBoard,
  TicketList,
  Workspace,
  WorkspacePermissions,
} from '../api/types';
import {
  DASHBOARD_FILTERS,
  dashboardQuery,
  parseDashboardQuery,
  parseHash,
  type DashboardParams,
  type Route,
  type TicketView,
} from './routes';
import { ScopedLoader, type ScopedState } from './scopedLoader';

export type Boot =
  | { status: 'loading' }
  | { status: 'ready'; meta: Meta; session: Session; workspaces: Workspace[] }
  | { status: 'no_access'; meta: Meta }
  /** US-1.2 (TM-02 b): a role that needs MFA without an authenticator may only enrol (and log out). */
  | { status: 'mfa_enrol'; meta: Meta; session: Session }
  /** US-1.4: only the change-password screen (and logout) until the password is changed. */
  | { status: 'must_change'; meta: Meta; session: Session }
  | { status: 'error'; error: unknown };

/**
 * Meta → existing session or (UAT only) direct Superadmin entry → workspace list. A session that
 * must change its password stops before the workspace list (the server refuses it with 403).
 */
export async function loadBoot(api: Api, signal: AbortSignal): Promise<Boot> {
  const meta = await api.getMeta(signal);
  // No local login exists: outside UAT the app waits for office SSO (not configured).
  if (meta.accessMode !== 'password' && meta.accessMode !== 'uat_superadmin') return { status: 'no_access', meta };
  let session: Session;
  try {
    session = await api.getSession(signal);
  } catch (e) {
    if (!(e instanceof ApiError && e.status === 401)) throw e;
    if (meta.accessMode === 'password') return { status: 'no_access', meta };
    session = await api.enterUat(signal);
  }
  // MFA first, then the forced password change: the server checks them in that order.
  if (meta.accessMode === 'password' && session.mfaRequired && !session.mfaEnrolled) return { status: 'mfa_enrol', meta, session };
  if (session.mustChangePassword) return { status: 'must_change', meta, session };
  const workspaces = await api.listWorkspaces(signal);
  return { status: 'ready', meta, session, workspaces };
}

/** `loadBoot` once per attempt; `retry` re-reads the session (after login, logout or a 401). */
export function useBoot(api: Api) {
  const [boot, setBoot] = useState<Boot>({ status: 'loading' });
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    const ctl = new AbortController();
    setBoot({ status: 'loading' });
    loadBoot(api, ctl.signal).then((b) => { if (!ctl.signal.aborted) setBoot(b); }).catch((error: unknown) => {
      if (!isAbort(error)) setBoot({ status: 'error', error });
    });
    return () => ctl.abort();
  }, [api, attempt]);

  const retry = useCallback(() => setAttempt((n) => n + 1), []);
  return { boot, retry };
}

export interface ScopeData {
  workspace: Workspace;
  permissions: WorkspacePermissions;
}

/** Workspace-scoped data; a switch aborts the old request and never shows old data. */
export function useScope(api: Api, workspaceId: string | null, attempt = 0): ScopedState<string, ScopeData> {
  const loader = useRef<ScopedLoader<string, ScopeData> | null>(null);
  loader.current ??= new ScopedLoader<string, ScopeData>(isAbort);
  const l = loader.current;
  const state = useSyncExternalStore(l.subscribe, l.getState);

  useEffect(() => {
    if (!workspaceId) {
      l.reset();
      return;
    }
    l.load(workspaceId, async (signal) => {
      const [workspace, permissions] = await Promise.all([
        api.getWorkspace(workspaceId, signal),
        api.getMyPermissions(workspaceId, signal),
      ]);
      return { workspace, permissions };
    });
  }, [api, l, workspaceId, attempt]);

  // A state for another key is stale; present it as loading.
  if (state.status !== 'idle' && workspaceId && state.key !== workspaceId) {
    return { status: 'loading', key: workspaceId };
  }
  return state;
}

/**
 * Dashboard aggregates for (workspace, period, filters). Same guarantees as useScope: a switch
 * aborts the previous request, a late answer for another key is ignored and old numbers are
 * never shown under the new key.
 */
export function useDashboard(
  api: Api,
  workspaceId: string,
  params: DashboardParams,
  attempt = 0,
): ScopedState<string, Dashboard> {
  const loader = useRef<ScopedLoader<string, Dashboard> | null>(null);
  loader.current ??= new ScopedLoader<string, Dashboard>(isAbort);
  const l = loader.current;
  const state = useSyncExternalStore(l.subscribe, l.getState);
  const query = dashboardQuery(params);
  const key = dashboardKey(workspaceId, query);

  useEffect(() => {
    const p = parseDashboardQuery(query);
    l.load(key, async (signal) => {
      const d = await api.getDashboard(workspaceId, query, signal);
      // Defence in depth: never render numbers that belong to another scope, period or filter.
      if (d.workspaceId !== workspaceId || d.period.key !== p.period ||
        DASHBOARD_FILTERS.some((k) => d.filters[k] !== p.filters[k])) {
        throw new ApiError(0, 'bad_response', 'Respons dashboard tidak cocok dengan workspace/periode/filter.');
      }
      return d;
    });
  }, [api, l, key, workspaceId, query, attempt]);
  useEffect(() => () => l.reset(), [l]);

  if (state.status === 'idle' || state.key !== key) return { status: 'loading', key };
  return state;
}

/** Key of one dashboard request: workspace + canonical query (or a bare period key). */
export const dashboardKey = (workspaceId: string, query: string) => `${workspaceId}|${query}`;

/**
 * Hash routing with a leave guard. The app renders the *committed* hash; a change that the guard
 * refuses (a dirty intake draft would leave view) is held as `pending` and the URL is put back,
 * so neither a nav link nor Back/Forward drops the draft silently. Query-only changes never ask.
 */
export type NavGuard = (from: Route, to: Route) => boolean;

const router = {
  committed: null as string | null,
  guard: null as NavGuard | null,
  pending: null as string | null,
  listeners: new Set<() => void>(),
  installed: false,
};

export const currentHash = () => router.committed ?? (router.committed = window.location.hash);
const emit = () => router.listeners.forEach((fn) => fn());
const blocks = (from: string, to: string) => !!router.guard && router.guard(parseHash(from), parseHash(to));

function onHashChange() {
  const to = window.location.hash;
  const from = currentHash();
  if (to === from) return;
  if (blocks(from, to)) {
    // Back/Forward or a typed URL: restore the committed entry and ask first.
    window.history.pushState(null, '', `${window.location.pathname}${window.location.search}${from || '#'}`);
    router.pending = to;
  } else {
    router.committed = to;
  }
  emit();
}

export const subscribeRoute = (fn: () => void) => {
  if (!router.installed) {
    router.installed = true;
    currentHash();
    window.addEventListener('hashchange', onHashChange);
  }
  router.listeners.add(fn);
  return () => { router.listeners.delete(fn); };
};

export function useHashRoute(): Route {
  const hash = useSyncExternalStore(subscribeRoute, currentHash);
  // parseHash returns a fresh object; memoise on the string.
  const memo = useRef<{ hash: string; route: Route } | null>(null);
  if (!memo.current || memo.current.hash !== hash) memo.current = { hash, route: parseHash(hash) };
  return memo.current.route;
}

/** Install the leave guard (one at a time); returns the uninstaller. */
export function setNavGuard(guard: NavGuard): () => void {
  router.guard = guard;
  return () => { if (router.guard === guard) router.guard = null; };
}

/** Target held back by the guard, or null. */
export function usePendingNav(): string | null {
  return useSyncExternalStore(subscribeRoute, () => router.pending);
}

/** Answer the guard: go on to the held target, or stay (the draft is untouched either way). */
export function resolvePendingNav(proceed: boolean) {
  const to = router.pending;
  router.pending = null;
  if (proceed && to !== null) navigate(to, false, true);
  else emit();
}

/**
 * Go to `hash`. Returns false when the guard held it back for a decision (`force` skips the
 * guard: the user already decided).
 */
export function navigate(hash: string, replace = false, force = false): boolean {
  const from = currentHash();
  if (!force && hash !== from && blocks(from, hash)) {
    router.pending = hash;
    emit();
    return false;
  }
  router.committed = hash;
  if (replace) window.history.replaceState(null, '', hash);
  else if (window.location.hash !== hash) window.location.hash = hash;
  emit();
  return true;
}

export type TicketData = { view: 'list'; list: TicketList } | { view: 'board'; board: TicketBoard };

/**
 * First List page or the Board for (workspace, view, canonical API query). A new key aborts the
 * old request; an answer for another key is never shown. Further pages are loaded by the page
 * with the returned cursors.
 */
export function useTicketData(
  api: Api,
  workspaceId: string,
  view: TicketView,
  query: string,
  attempt = 0,
): ScopedState<string, TicketData> {
  const loader = useRef<ScopedLoader<string, TicketData> | null>(null);
  loader.current ??= new ScopedLoader<string, TicketData>(isAbort);
  const l = loader.current;
  const state = useSyncExternalStore(l.subscribe, l.getState);
  const key = `${workspaceId}|${view}|${query}|${attempt}`;

  useEffect(() => {
    l.load(key, async (signal): Promise<TicketData> => {
      const want = new URLSearchParams(query);
      const echoed = (f: { q: string | null; sort: string; order: string }) =>
        (f.q ?? '') === (want.get('q') ?? '') && f.sort === (want.get('sort') ?? 'updated_at') &&
        f.order === (want.get('order') ?? 'desc');
      if (view === 'board') {
        const board = await api.getBoard(workspaceId, query, signal);
        if (!echoed(board.filters)) throw new ApiError(0, 'bad_response', 'Respons board tidak cocok dengan filter.');
        return { view, board };
      }
      const list = await api.listTickets(workspaceId, query, null, signal);
      if (!echoed(list.filters)) throw new ApiError(0, 'bad_response', 'Respons daftar tidak cocok dengan filter.');
      return { view, list };
    });
  }, [api, l, key, workspaceId, view, query]);
  useEffect(() => () => l.reset(), [l]);

  if (state.status === 'idle' || state.key !== key) return { status: 'loading', key };
  return state;
}

export interface TicketOptions {
  owners: Member[];
  channels: CatalogItem[];
  categories: CatalogItem[];
  priorities: CatalogItem[];
}

/**
 * Filter choices of one workspace (members + catalogs). A refused or failed source leaves that
 * list empty: the filter still works from the URL and the server validates it.
 */
export function useTicketOptions(api: Api, workspaceId: string): TicketOptions | null {
  const [opts, setOpts] = useState<{ key: string; value: TicketOptions } | null>(null);
  useEffect(() => {
    const ctl = new AbortController();
    const soft = <T,>(p: Promise<T>, empty: T) => p.catch((e: unknown) => {
      if (isAbort(e)) throw e;
      return empty;
    });
    Promise.all([
      soft(api.listMembers(workspaceId, ctl.signal), [] as Member[]),
      soft(api.getCatalogs(workspaceId, ctl.signal).then((c) => c), null),
    ]).then(([owners, c]) => setOpts({
      key: workspaceId,
      value: {
        owners,
        channels: c?.intakeChannels ?? [],
        categories: c?.categories ?? [],
        priorities: c?.priorities ?? [],
      },
    }), () => undefined);
    return () => ctl.abort();
  }, [api, workspaceId]);
  return opts && opts.key === workspaceId ? opts.value : null;
}
