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
}
// Any other workspace path replays the server's real 404 (unknown/foreign/malformed look alike).
responses['GET #not_found'] = await rec('GET', '/api/v1/workspaces/00000000-0000-4000-8000-000000000000');

const fixtures = { recordedAt: new Date().toISOString(), origin: BASE, backendSha: process.env.BACKEND_SHA ?? null, responses };
writeFileSync(OUT, JSON.stringify(fixtures, null, 2) + '\n');
console.log(`recorded ${Object.keys(responses).length} responses -> ${OUT.pathname}`);
