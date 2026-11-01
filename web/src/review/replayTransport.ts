import type { Transport } from '../api/client';

export interface Recorded { status: number; body: unknown; requestId: string | null }
export interface Fixtures { recordedAt: string; origin: string; backendSha: string | null; responses: Record<string, Recorded> }

/**
 * Review-only state scenarios for the dashboard request. Everything except `none` is synthetic and
 * labelled as such in the review note; the recorded responses themselves are never edited.
 * - slow: dashboard answers after 4 s (loading skeleton)
 * - empty: dashboard with every count at zero (empty state)
 * - error: 503 from the dashboard (retryable error)
 * - forbidden: 403 from the dashboard
 * - race: the first workspace's dashboard is slow and ignores cancellation, so its answer lands
 *   after a switch to another workspace; the UI must discard it.
 */
export type Scenario = 'none' | 'slow' | 'empty' | 'error' | 'forbidden' | 'race';
export const SCENARIOS: readonly Scenario[] = ['none', 'slow', 'empty', 'error', 'forbidden', 'race'];

const DASHBOARD = /^\/api\/v1\/workspaces\/([^/]+)\/dashboard\?/;

function zeroed(body: unknown): unknown {
  const d = structuredClone(body) as Record<string, unknown>;
  const totals = d.totals as Record<string, number>;
  for (const k of Object.keys(totals)) totals[k] = 0;
  const counts = (rows: unknown, keys: string[]) =>
    (rows as Record<string, unknown>[]).map((r) => ({ ...r, ...Object.fromEntries(keys.map((k) => [k, 0])) }));
  d.statusDistribution = counts(d.statusDistribution, ['count']);
  d.picLoad = [];
  d.channels = [];
  d.age = counts(d.age, ['count']);
  d.trend = counts(d.trend, ['received', 'resolved']);
  d.attention = { total: 0, items: [] };
  return d;
}

function wait(ms: number, signal: AbortSignal | undefined, honourAbort: boolean): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    const t = setTimeout(resolve, ms);
    if (honourAbort) {
      signal?.addEventListener('abort', () => {
        clearTimeout(t);
        reject(new DOMException('Aborted', 'AbortError'));
      }, { once: true });
    }
  });
}

/**
 * Review-only transport: replays responses recorded from the real UAT backend so the shell can
 * be reviewed from a file:// page. Session state is simulated (anonymous until UAT entry).
 */
export function replayTransport(f: Fixtures, latencyMs = 250, scenario: Scenario = 'none'): Transport {
  let entered = false;
  let firstDashboardWs: string | null = null;
  return async ({ method, path, signal }) => {
    const dash = DASHBOARD.exec(path);
    if (dash && firstDashboardWs === null) firstDashboardWs = dash[1] ?? null;
    const lagging = scenario === 'race' && dash !== null && dash[1] === firstDashboardWs;
    const delay = dash && scenario === 'slow' ? 4000 : lagging ? 3000 : latencyMs;
    await wait(delay, signal, !lagging);
    const key = `${method} ${path}`;
    if (key === 'POST /api/v1/uat/session') entered = true;
    const hit = key === 'GET /api/v1/session' && !entered
      ? f.responses['GET /api/v1/session#anonymous']
      : f.responses[key] ?? (path.startsWith('/api/v1/workspaces/') ? f.responses['GET #not_found'] : undefined);
    if (dash && scenario === 'error') {
      return { status: 503, body: { error: { code: 'dependency_unavailable', message: 'Simulasi review: layanan tidak tersedia.' } }, requestId: 'review-simulasi' };
    }
    if (dash && scenario === 'forbidden') {
      return { status: 403, body: { error: { code: 'forbidden', message: 'Simulasi review: tidak diizinkan.' } }, requestId: 'review-simulasi' };
    }
    if (!hit) return { status: 404, body: null, requestId: null };
    const body = dash && scenario === 'empty' && hit.status === 200 ? zeroed(hit.body) : structuredClone(hit.body);
    return { status: hit.status, body, requestId: hit.requestId };
  };
}
