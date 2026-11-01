import type { Transport } from '../api/client';

export interface Recorded { status: number; body: unknown; requestId: string | null }
export interface Fixtures { recordedAt: string; origin: string; backendSha: string | null; responses: Record<string, Recorded> }

/**
 * Review-only transport: replays responses recorded from the real UAT backend so the shell can
 * be reviewed from a file:// page. Session state is simulated (anonymous until UAT entry).
 */
export function replayTransport(f: Fixtures, latencyMs = 250): Transport {
  let entered = false;
  return async ({ method, path, signal }) => {
    await new Promise<void>((resolve, reject) => {
      const t = setTimeout(resolve, latencyMs);
      signal?.addEventListener('abort', () => {
        clearTimeout(t);
        reject(new DOMException('Aborted', 'AbortError'));
      }, { once: true });
    });
    const key = `${method} ${path}`;
    if (key === 'POST /api/v1/uat/session') entered = true;
    const hit = key === 'GET /api/v1/session' && !entered
      ? f.responses['GET /api/v1/session#anonymous']
      : f.responses[key] ?? (path.startsWith('/api/v1/workspaces/') ? f.responses['GET #not_found'] : undefined);
    if (!hit) return { status: 404, body: null, requestId: null };
    return { status: hit.status, body: structuredClone(hit.body), requestId: hit.requestId };
  };
}
