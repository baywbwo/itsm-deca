#!/usr/bin/env node
// Records real responses from a running UAT backend (dummy data) for the static review
// package. The CSRF token and cookies are never written; the token is replaced by a marker.
import { writeFileSync } from 'node:fs';

const BASE = process.env.ITSM_WEB_ORIGIN ?? 'http://127.0.0.1:4183';
const OUT = new URL('../src/review/fixtures.json', import.meta.url);
let cookie = '';

async function rec(method, path, headers = {}) {
  const res = await fetch(BASE + path, { method, headers: { Accept: 'application/json', ...(cookie && { Cookie: cookie }), ...headers } });
  const set = res.headers.getSetCookie();
  if (set.length) cookie = set.map((c) => c.split(';')[0]).join('; ');
  const text = await res.text();
  return { status: res.status, body: text ? JSON.parse(text) : null, requestId: res.headers.get('x-request-id') };
}

const responses = {};
responses['GET /api/v1/meta'] = await rec('GET', '/api/v1/meta');
responses['GET /api/v1/session#anonymous'] = await rec('GET', '/api/v1/session');
const entry = await rec('POST', '/api/v1/uat/session', { 'X-ITSM-UAT-Entry': '1' });
if (entry.status !== 201) throw new Error(`UAT entry failed: ${entry.status}`);
entry.body.csrfToken = 'rekaman-token-disensor';
responses['POST /api/v1/uat/session'] = entry;
responses['GET /api/v1/session'] = await rec('GET', '/api/v1/session');
const list = await rec('GET', '/api/v1/workspaces');
responses['GET /api/v1/workspaces'] = list;
for (const w of list.body.items) {
  responses[`GET /api/v1/workspaces/${w.id}`] = await rec('GET', `/api/v1/workspaces/${w.id}`);
  responses[`GET /api/v1/workspaces/${w.id}/permissions`] = await rec('GET', `/api/v1/workspaces/${w.id}/permissions`);
  let options = null;
  for (const period of ['today', 'last_7_days', 'this_month']) {
    const path = `/api/v1/workspaces/${w.id}/dashboard?period=${period}`;
    responses[`GET ${path}`] = await rec('GET', path);
    options ??= responses[`GET ${path}`].body.filterOptions;
  }
  // Every single filter value (plus the unset word) for every period, in the canonical query
  // order the app sends. Combinations are not recorded; the replay says so instead of guessing.
  const singles = [
    ...options.owners.map((o) => ['owner', o.id]), ['owner', 'unassigned'],
    ...options.channels.map((o) => ['channel', o.id]), ['channel', 'none'],
    ...options.categories.map((o) => ['category', o.id]), ['category', 'none'],
    ...options.priorities.map((o) => ['priority', o.id]), ['priority', 'none'],
  ];
  for (const period of ['today', 'last_7_days', 'this_month']) {
    for (const [k, v] of singles) {
      const path = `/api/v1/workspaces/${w.id}/dashboard?${new URLSearchParams({ period, [k]: v })}`;
      responses[`GET ${path}`] = await rec('GET', path);
    }
  }
  // A few combined filters for the visual review (today only).
  const urgent = options.priorities.find((p) => p.code === 'urgent');
  const pairs = [
    options.owners[0] && urgent && { owner: options.owners[0].id, priority: urgent.id },
    options.channels[0] && options.categories[0] && { channel: options.channels[0].id, category: options.categories[0].id },
  ].filter(Boolean);
  for (const p of pairs) {
    const q = new URLSearchParams({ period: 'today' });
    for (const k of ['owner', 'channel', 'category', 'priority']) if (p[k]) q.set(k, p[k]);
    const path = `/api/v1/workspaces/${w.id}/dashboard?${q}`;
    responses[`GET ${path}`] = await rec('GET', path);
  }
  // A filter value from outside the workspace: the server's real 400.
  const bad = `/api/v1/workspaces/${w.id}/dashboard?period=today&owner=00000000-0000-4000-8000-000000000000`;
  responses[`GET ${bad}`] = await rec('GET', bad);
}
// Any other workspace path replays the server's real 404 (unknown/foreign/malformed look alike).
responses['GET #not_found'] = await rec('GET', '/api/v1/workspaces/00000000-0000-4000-8000-000000000000');

const fixtures = { recordedAt: new Date().toISOString(), origin: BASE, backendSha: process.env.BACKEND_SHA ?? null, responses };
writeFileSync(OUT, JSON.stringify(fixtures, null, 2) + '\n');
console.log(`recorded ${Object.keys(responses).length} responses -> ${OUT.pathname}`);
