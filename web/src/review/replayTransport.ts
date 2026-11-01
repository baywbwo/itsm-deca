import { ApiError, type Transport } from '../api/client';

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
 * - conflict: a status change replays the server's real 409 (recorded with a stale version).
 * The ticket List/Board follows slow/empty/error/forbidden the same way as the dashboard.
 * Saved views (C32) are always synthetic in the review package: an in-memory store per page
 * load (lost on reload, never in browser storage) that mimics the server rules (unique name,
 * version 409, 404). It does not prove the live API; that is covered by the cargo/UAT tests.
 * D07 intake replays real recorded answers: the first key gets the recorded 201, the same key with
 * the same payload the recorded replay (200), the same key with another payload the recorded 409.
 * - intake-unknown: the first send fails as a network error (synthetic), the resend replays.
 * - intake-degraded: the create answer recorded while the simulation was Jira unknown / Lark failed.
 * - conflict: also the PIC change replays the server's real 409 (stale version).
 * Retry/reconcile and other combinations are not recorded; the replay says so.
 */
export type Scenario = 'none' | 'slow' | 'empty' | 'error' | 'forbidden' | 'race' | 'conflict' | 'intake-unknown' | 'intake-degraded';
export const SCENARIOS: readonly Scenario[] = ['none', 'slow', 'empty', 'error', 'forbidden', 'race', 'conflict', 'intake-unknown', 'intake-degraded'];

const DASHBOARD = /^\/api\/v1\/workspaces\/([^/]+)\/dashboard\?/;
const TICKETS = /^\/api\/v1\/workspaces\/[^/]+\/tickets(\/board)?(\?|$)/;

function emptied(body: unknown): unknown {
  const d = structuredClone(body) as Record<string, unknown>;
  d.total = 0;
  if ('items' in d) Object.assign(d, { items: [], countsByStatus: [], nextCursor: null });
  if ('columns' in d) d.columns = [];
  if (d.resolutionEvents !== null) d.resolutionEvents = 0;
  return d;
}

function zeroed(body: unknown): unknown {
  const d = structuredClone(body) as Record<string, unknown>;
  const totals = d.totals as Record<string, number>;
  for (const k of Object.keys(totals)) totals[k] = 0;
  const counts = (rows: unknown, keys: string[]) =>
    (rows as Record<string, unknown>[]).map((r) => ({ ...r, ...Object.fromEntries(keys.map((k) => [k, 0])) }));
  d.statusDistribution = counts(d.statusDistribution, ['count']);
  d.picLoad = [];
  d.channels = [];
  d.categories = [];
  d.age = counts(d.age, ['count']);
  d.trend = counts(d.trend, ['received', 'resolved']);
  d.attention = { rule: 'operational_rule', total: 0, limit: 10, items: [] };
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
const CREATE = /^\/api\/v1\/workspaces\/[^/]+\/tickets$/;
const ASSIGN = /^\/api\/v1\/workspaces\/[^/]+\/tickets\/[^/]+\/assignment$/;
const notRecorded = (what: string) => ({
  status: 503,
  body: { error: { code: 'dependency_unavailable', message: `Paket review: ${what} tidak direkam.` } },
  requestId: 'review-tidak-direkam',
});

const SAVED = /^\/api\/v1\/workspaces\/([^/]+)\/saved-views(?:\/([^/]+))?$/;

interface SyntheticView { id: string; name: string; view: string; query: Record<string, string>; version: number; createdAt: string; updatedAt: string }

function savedViews(store: Map<string, SyntheticView[]>, method: string, ws: string, id: string | undefined, payload: unknown) {
  const items = store.get(ws) ?? [];
  const fail = (status: number, code: string, message: string) =>
    ({ status, body: { error: { code, message } }, requestId: 'review-simulasi-saved-view' });
  const ok = (status: number, body: unknown) => ({ status, body, requestId: 'review-simulasi-saved-view' });
  const p = (payload ?? {}) as { name?: string; view?: string; query?: Record<string, string>; version?: number };
  const name = (p.name ?? '').trim();
  const dup = (except?: string) => items.some((v) => v.id !== except && v.name.toLowerCase() === name.toLowerCase());
  const now = new Date().toISOString();
  if (!id && method === 'GET') return ok(200, { items: structuredClone(items) });
  if (!id && method === 'POST') {
    if (!name || name.length > 60) return fail(400, 'invalid_request', 'Simulasi review: nama tidak valid.');
    if (dup() || items.length >= 50) return fail(422, 'rule_violation', 'Simulasi review: nama dipakai atau batas tercapai.');
    const v = { id: `sim-${items.length + 1}-${Date.now()}`, name, view: p.view ?? 'list', query: { ...(p.query ?? {}) }, version: 1, createdAt: now, updatedAt: now };
    store.set(ws, [...items, v]);
    return ok(201, structuredClone(v));
  }
  const cur = items.find((v) => v.id === id);
  if (!cur) return fail(404, 'not_found', 'Data tidak ditemukan.');
  if (method === 'PATCH') {
    if (p.version !== cur.version) return fail(409, 'version_conflict', 'Simulasi review: versi berubah.');
    if (!name || name.length > 60) return fail(400, 'invalid_request', 'Simulasi review: nama tidak valid.');
    if (dup(cur.id)) return fail(422, 'rule_violation', 'Simulasi review: nama dipakai.');
    const v = { ...cur, name, version: cur.version + 1, updatedAt: now };
    store.set(ws, items.map((x) => (x.id === id ? v : x)));
    return ok(200, structuredClone(v));
  }
  if (method === 'DELETE') {
    store.set(ws, items.filter((x) => x.id !== id));
    return ok(204, null);
  }
  return fail(400, 'invalid_request', 'Simulasi review: metode tidak didukung.');
}

export function replayTransport(f: Fixtures, latencyMs = 250, scenario: Scenario = 'none'): Transport {
  let entered = false;
  let firstDashboardWs: string | null = null;
  let mutated = false;
  const saved = new Map<string, SyntheticView[]>();
  const keys = new Map<string, string>(); // idempotency key -> payload JSON
  let networkFailed = false;
  const after = new Set<string>(); // GET keys that now answer with their '#after' recording
  return async ({ method, path, signal, headers, body: payload }) => {
    if (method === 'POST' && CREATE.test(path)) {
      await wait(latencyMs, signal, true);
      const key = headers['Idempotency-Key'] ?? '';
      const json = JSON.stringify(payload);
      const seen = keys.get(key);
      let variant: string;
      if (seen === undefined) {
        if (keys.size > 0) return notRecorded('tiket kedua dengan kunci baru');
        if (scenario === 'intake-unknown' && !networkFailed) {
          networkFailed = true;
          keys.set(key, json);
          throw new ApiError(0, 'network', 'Simulasi review: koneksi terputus sebelum jawaban server diterima.');
        }
        keys.set(key, json);
        variant = scenario === 'intake-degraded' ? 'degraded' : 'created';
      } else if (seen !== json) {
        variant = 'conflict';
      } else {
        // Same key + same payload (also the resend after a lost answer): the server replays.
        variant = 'replayed';
        if (scenario === 'intake-degraded') return notRecorded('pengiriman ulang pada skenario ini');
      }
      const hit = f.responses[`POST ${path}#${variant}`];
      if (!hit) return notRecorded('pembuatan tiket di workspace ini');
      return { status: hit.status, body: structuredClone(hit.body), requestId: hit.requestId };
    }
    if (method === 'POST' && ASSIGN.test(path)) {
      await wait(latencyMs, signal, true);
      const hit = f.responses[`POST ${path}#${scenario === 'conflict' ? 'stale' : 'ok'}`];
      if (!hit) return notRecorded('perubahan PIC untuk tiket ini');
      if (hit.status === 200) after.add(`GET ${path}-history`);
      return { status: hit.status, body: structuredClone(hit.body), requestId: hit.requestId };
    }
    if (method === 'POST' && /\/integration-outbox\/[^/]+\/(retry|reconcile)$/.test(path)) {
      await wait(latencyMs, signal, true);
      return notRecorded('pemulihan integrasi (coba ulang/rekonsiliasi)');
    }
    if (method === 'GET' && after.has(`GET ${path}`) && f.responses[`GET ${path}#after`]) {
      await wait(latencyMs, signal, true);
      const hit = f.responses[`GET ${path}#after`]!;
      return { status: hit.status, body: structuredClone(hit.body), requestId: hit.requestId };
    }
    const sv = SAVED.exec(path);
    if (sv) {
      await wait(latencyMs, signal, true);
      return savedViews(saved, method, sv[1] ?? '', sv[2], payload);
    }
    if (method === 'POST' && path.endsWith('/transitions')) {
      await wait(latencyMs, signal, true);
      const to = (payload as { to?: string } | undefined)?.to ?? '';
      const hit = f.responses[`POST ${path}#${scenario === 'conflict' ? 'stale' : to}`];
      if (!hit) {
        return {
          status: 503,
          body: { error: { code: 'dependency_unavailable', message: 'Paket review: perubahan status ini tidak direkam.' } },
          requestId: 'review-tidak-direkam',
        };
      }
      if (hit.status === 200) mutated = true;
      return { status: hit.status, body: structuredClone(hit.body), requestId: hit.requestId };
    }
    const tickets = TICKETS.test(path);
    const dash = DASHBOARD.exec(path) ?? (tickets ? ([path, ''] as unknown as RegExpExecArray) : null);
    if (dash && !tickets && firstDashboardWs === null) firstDashboardWs = dash[1] ?? null;
    const lagging = scenario === 'race' && !tickets && dash !== null && dash[1] === firstDashboardWs;
    const delay = dash && scenario === 'slow' ? 4000 : lagging ? 3000 : latencyMs;
    await wait(delay, signal, !lagging);
    const key = `${method} ${path}`;
    if (key === 'POST /api/v1/uat/session') entered = true;
    const hit = key === 'GET /api/v1/session' && !entered
      ? f.responses['GET /api/v1/session#anonymous']
      : (mutated ? f.responses[`${key}#after`] : undefined) ?? f.responses[key] ?? (path.startsWith('/api/v1/workspaces/') ? f.responses['GET #not_found'] : undefined);
    if (dash && scenario === 'error') {
      return { status: 503, body: { error: { code: 'dependency_unavailable', message: 'Simulasi review: layanan tidak tersedia.' } }, requestId: 'review-simulasi' };
    }
    if (dash && scenario === 'forbidden') {
      return { status: 403, body: { error: { code: 'forbidden', message: 'Simulasi review: tidak diizinkan.' } }, requestId: 'review-simulasi' };
    }
    if (dash && !f.responses[key]) {
      // Only single filters are recorded; a combination is not invented from other answers.
      return {
        status: 503,
        body: { error: { code: 'dependency_unavailable', message: 'Paket review: kombinasi filter/halaman ini tidak direkam.' } },
        requestId: 'review-tidak-direkam',
      };
    }
    if (!hit) return { status: 404, body: null, requestId: null };
    const body = dash && scenario === 'empty' && hit.status === 200
      ? (tickets ? emptied(hit.body) : zeroed(hit.body))
      : structuredClone(hit.body);
    return { status: hit.status, body, requestId: hit.requestId };
  };
}
