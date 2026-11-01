#!/usr/bin/env node
// Live integration check of the API surface the D04 shell uses, through the same origin the
// browser would use (vite preview proxy by default). Dummy UAT data only. Never prints the
// session cookie value or the CSRF token.
import assert from 'node:assert/strict';

const BASE = process.env.ITSM_WEB_ORIGIN ?? 'http://127.0.0.1:4183';
const jar = new Map();
const results = [];

async function req(method, path, { headers = {}, cookies = true } = {}) {
  const h = { Accept: 'application/json', ...headers };
  if (cookies && jar.size) h.Cookie = [...jar].map(([k, v]) => `${k}=${v}`).join('; ');
  const res = await fetch(BASE + path, { method, headers: h, redirect: 'manual' });
  for (const c of res.headers.getSetCookie()) {
    const [pair] = c.split(';');
    const i = pair.indexOf('=');
    const name = pair.slice(0, i).trim();
    const value = pair.slice(i + 1).trim();
    if (value === '' || /max-age=0/i.test(c)) jar.delete(name);
    else jar.set(name, value);
  }
  const text = await res.text();
  return { status: res.status, body: text ? JSON.parse(text) : null, res };
}

async function step(name, fn) {
  try {
    await fn();
    results.push(['PASS', name]);
  } catch (e) {
    results.push(['FAIL', `${name}: ${e.message}`]);
  }
}

let csrf = null;
let workspaces = [];

await step('GET /api/v1/meta exposes the persistent UAT notice', async () => {
  const r = await req('GET', '/api/v1/meta');
  assert.equal(r.status, 200);
  assert.equal(r.body.uat?.banner, 'Mode uji — Superadmin');
  assert.equal(r.body.uat?.persistent, true);
  assert.equal(r.body.uat?.superadminProvesRbac, false);
});
await step('GET /api/v1/session without cookie is 401 unauthenticated', async () => {
  const r = await req('GET', '/api/v1/session', { cookies: false });
  assert.equal(r.status, 401);
  assert.equal(r.body.error.code, 'unauthenticated');
});
await step('POST /api/v1/uat/session without entry header is refused', async () => {
  const r = await req('POST', '/api/v1/uat/session', { cookies: false });
  assert.ok(r.status >= 400 && r.status < 500, `status ${r.status}`);
  assert.equal(jar.size, 0);
});
await step('POST /api/v1/uat/session issues HttpOnly SameSite=Strict cookie + CSRF token once', async () => {
  const r = await req('POST', '/api/v1/uat/session', { headers: { 'X-ITSM-UAT-Entry': '1' }, cookies: false });
  assert.equal(r.status, 201);
  assert.equal(typeof r.body.csrfToken, 'string');
  assert.equal(r.body.uatSuperadmin, true);
  const set = r.res.headers.getSetCookie().join('\n');
  assert.match(set, /HttpOnly/i);
  assert.match(set, /SameSite=Strict/i);
  csrf = r.body.csrfToken;
  console.log(`  cookie names: ${[...jar.keys()].join(', ')} (values not printed)`);
});
await step('GET /api/v1/session returns the user and no CSRF token', async () => {
  const r = await req('GET', '/api/v1/session');
  assert.equal(r.status, 200);
  assert.equal(r.body.uatSuperadmin, true);
  assert.equal('csrfToken' in r.body, false);
});
await step('GET /api/v1/workspaces lists the seeded dummy workspaces', async () => {
  const r = await req('GET', '/api/v1/workspaces');
  assert.equal(r.status, 200);
  workspaces = r.body.items;
  const codes = workspaces.map((w) => w.code).sort();
  assert.deepEqual(codes, ['DUMA', 'DUMB']);
  console.log(`  workspaces: ${workspaces.map((w) => `${w.code}=${w.name}`).join('; ')}`);
});
await step('GET workspace + permissions per workspace (switcher data)', async () => {
  for (const w of workspaces) {
    const one = await req('GET', `/api/v1/workspaces/${w.id}`);
    assert.equal(one.status, 200);
    assert.equal(one.body.id, w.id);
    const p = await req('GET', `/api/v1/workspaces/${w.id}/permissions`);
    assert.equal(p.status, 200);
    assert.equal(p.body.workspaceId, w.id);
    assert.ok(Array.isArray(p.body.permissions) && p.body.permissions.includes('ticket.read'));
    console.log(`  ${w.code}: role=${p.body.role} uatSuperadmin=${p.body.uatSuperadmin} permissions=${p.body.permissions.length} grants=${p.body.grants.length}`);
  }
});
await step('Unknown and malformed workspace IDs both give the same 404', async () => {
  const a = await req('GET', '/api/v1/workspaces/00000000-0000-4000-8000-000000000000');
  const b = await req('GET', '/api/v1/workspaces/bukan-uuid');
  const c = await req('GET', '/api/v1/workspaces/00000000-0000-4000-8000-000000000000/permissions');
  for (const r of [a, b, c]) {
    assert.equal(r.status, 404);
    assert.equal(r.body.error.code, 'not_found');
  }
  assert.equal(a.body.error.message, b.body.error.message);
});
await step('DELETE /api/v1/session without X-CSRF-Token is rejected and the session survives', async () => {
  const r = await req('DELETE', '/api/v1/session');
  assert.equal(r.status, 403);
  assert.equal(r.body.error.code, 'csrf_failed');
  const s = await req('GET', '/api/v1/session');
  assert.equal(s.status, 200);
});
await step('DELETE /api/v1/session with a wrong X-CSRF-Token is rejected', async () => {
  const r = await req('DELETE', '/api/v1/session', { headers: { 'X-CSRF-Token': 'salah' } });
  assert.equal(r.status, 403);
  assert.equal(r.body.error.code, 'csrf_failed');
});
await step('DELETE /api/v1/session with the token ends the session (then 401)', async () => {
  const r = await req('DELETE', '/api/v1/session', { headers: { 'X-CSRF-Token': csrf } });
  assert.ok(r.status === 204 || r.status === 200, `status ${r.status}`);
  const s = await req('GET', '/api/v1/session');
  assert.equal(s.status, 401);
});
await step('Health endpoints are reachable through the same origin', async () => {
  const r = await req('GET', '/health/live', { cookies: false });
  assert.equal(r.status, 200);
});

for (const [s, n] of results) console.log(`${s} ${n}`);
const failed = results.filter(([s]) => s === 'FAIL').length;
console.log(`it-api: ${results.length - failed}/${results.length} pass (origin ${BASE})`);
process.exit(failed ? 1 : 0);
