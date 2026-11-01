import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { ApiError, isAbort, type Api } from '../api/client';
import type { Dashboard, DashboardPeriodKey, Meta, Session, Workspace, WorkspacePermissions } from '../api/types';
import { parseHash, type Route } from './routes';
import { ScopedLoader, type ScopedState } from './scopedLoader';

export type Boot =
  | { status: 'loading' }
  | { status: 'ready'; meta: Meta; session: Session; workspaces: Workspace[] }
  | { status: 'no_access'; meta: Meta }
  | { status: 'error'; error: unknown };

/** Meta → existing session or (UAT only) direct Superadmin entry → workspace list. */
export function useBoot(api: Api) {
  const [boot, setBoot] = useState<Boot>({ status: 'loading' });
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    const ctl = new AbortController();
    setBoot({ status: 'loading' });
    (async () => {
      const meta = await api.getMeta(ctl.signal);
      // No local login exists: outside UAT the app waits for office SSO (not configured).
      if (!meta.uat) return setBoot({ status: 'no_access', meta });
      let session: Session;
      try {
        session = await api.getSession(ctl.signal);
      } catch (e) {
        if (!(e instanceof ApiError && e.status === 401)) throw e;
        session = await api.enterUat(ctl.signal);
      }
      const workspaces = await api.listWorkspaces(ctl.signal);
      setBoot({ status: 'ready', meta, session, workspaces });
    })().catch((error: unknown) => {
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
 * Dashboard aggregates for (workspace, period). Same guarantees as useScope: a switch aborts
 * the previous request, a late answer for another key is ignored and old numbers are never
 * shown under the new key.
 */
export function useDashboard(
  api: Api,
  workspaceId: string,
  period: DashboardPeriodKey,
  attempt = 0,
): ScopedState<string, Dashboard> {
  const loader = useRef<ScopedLoader<string, Dashboard> | null>(null);
  loader.current ??= new ScopedLoader<string, Dashboard>(isAbort);
  const l = loader.current;
  const state = useSyncExternalStore(l.subscribe, l.getState);
  const key = dashboardKey(workspaceId, period);

  useEffect(() => {
    l.load(key, async (signal) => {
      const d = await api.getDashboard(workspaceId, period, signal);
      // Defence in depth: never render numbers that belong to another scope or period.
      if (d.workspaceId !== workspaceId || d.period.key !== period) {
        throw new ApiError(0, 'bad_response', 'Respons dashboard tidak cocok dengan workspace/periode.');
      }
      return d;
    });
  }, [api, l, key, workspaceId, period, attempt]);
  useEffect(() => () => l.reset(), [l]);

  if (state.status === 'idle' || state.key !== key) return { status: 'loading', key };
  return state;
}

export const dashboardKey = (workspaceId: string, period: DashboardPeriodKey) => `${workspaceId}|${period}`;

const subscribeHash = (fn: () => void) => {
  window.addEventListener('hashchange', fn);
  return () => window.removeEventListener('hashchange', fn);
};

export function useHashRoute(): Route {
  const hash = useSyncExternalStore(subscribeHash, () => window.location.hash);
  // parseHash returns a fresh object; memoise on the string.
  const memo = useRef<{ hash: string; route: Route } | null>(null);
  if (!memo.current || memo.current.hash !== hash) memo.current = { hash, route: parseHash(hash) };
  return memo.current.route;
}

export function navigate(hash: string, replace = false) {
  if (replace) window.history.replaceState(null, '', hash);
  else window.location.hash = hash;
  if (replace) window.dispatchEvent(new HashChangeEvent('hashchange'));
}
